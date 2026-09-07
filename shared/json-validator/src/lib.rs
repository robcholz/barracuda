//! `no_std + alloc` validation for statically compiled JSON Schemas.
//!
//! [`validator!`] reads a schema file during compilation. Unsupported keywords
//! are compile errors rather than silently ignored constraints. Runtime callers
//! can validate either an existing [`serde_json::Value`] or borrowed raw JSON;
//! raw validation walks borrowed fragments without building a JSON value tree.

#![no_std]

extern crate alloc;

use alloc::{borrow::Cow, format, string::String};
use core::fmt;

use serde::de::{Deserialize, Deserializer, Error as _, MapAccess, SeqAccess, Visitor};
use serde_json::{value::RawValue, Value};

pub use json_validator_macros::{rpc_validator, rpc_validator_source, validator, validator_source};

/// Static JSON Schema source paired with its compile-time validator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JsonSchema {
    source: &'static str,
    validator: Validator,
}

impl JsonSchema {
    /// Creates a static JSON Schema contract from its source and compiled validator.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_parts(source: &'static str, validator: Validator) -> Self {
        Self { source, validator }
    }

    /// Returns the embedded JSON Schema source.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.source
    }

    /// Validates one parsed JSON value.
    pub fn validate(self, value: &Value) -> Result<(), ValidationError> {
        self.validator.validate(value)
    }

    /// Validates one borrowed JSON document without constructing a value tree.
    pub fn validate_str(self, json: &str) -> Result<(), ValidationError> {
        self.validator.validate_str(json)
    }
}

/// Includes a manifest-relative JSON Schema and compiles its static validator.
#[macro_export]
macro_rules! json_schema {
    ($first:literal $(, $rest:literal)* $(,)?) => {
        $crate::JsonSchema::from_parts(
            ::core::include_str!(::core::concat!(
                ::core::env!("CARGO_MANIFEST_DIR"),
                "/",
                $first,
                $(
                    $rest,
                )*
            )),
            $crate::rpc_validator!($crate; $first $(, $rest)*),
        )
    };
}

/// Compiles one inline JSON Schema into a static contract.
#[macro_export]
macro_rules! json_schema_inline {
    ($source:literal $(,)?) => {
        $crate::JsonSchema::from_parts(
            $source,
            $crate::rpc_validator_source!($crate; $source),
        )
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Validator {
    schema: &'static Schema,
    definitions: &'static [Schema],
}

impl Validator {
    #[doc(hidden)]
    pub const fn from_schema(schema: &'static Schema) -> Self {
        Self {
            schema,
            definitions: &[],
        }
    }

    #[doc(hidden)]
    pub const fn from_parts(schema: &'static Schema, definitions: &'static [Schema]) -> Self {
        Self {
            schema,
            definitions,
        }
    }

    pub fn validate(&self, value: &Value) -> Result<(), ValidationError> {
        self.schema
            .validate_value(value, self.definitions)
            .map_err(ValidationError::with_root)
    }

    /// Validates a borrowed JSON document without constructing a [`Value`].
    pub fn validate_str(&self, json: &str) -> Result<(), ValidationError> {
        let raw = serde_json::from_str::<&RawValue>(json)
            .map_err(|_| ValidationError::new(ErrorKind::InvalidJson).with_root())?;
        self.schema
            .validate_raw(raw, self.definitions)
            .map_err(ValidationError::with_root)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Object,
    Array,
    String,
    Integer,
    Number,
    Boolean,
    Null,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Schema {
    reference: Option<usize>,
    value_types: &'static [Type],
    required: &'static [&'static str],
    properties: &'static [Property],
    additional_properties: bool,
    allowed_values: &'static [Literal],
    constant: Option<Literal>,
    alternatives: &'static [Schema],
    items: Option<&'static Schema>,
    constraints: Constraints,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Constraints {
    minimum: Option<f64>,
    maximum: Option<f64>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    min_items: Option<usize>,
    max_items: Option<usize>,
    max_properties: Option<usize>,
    pattern: Option<StringPattern>,
    format: Option<StringFormat>,
    content_encoding: Option<ContentEncoding>,
    trimmed_non_empty: bool,
}

impl Constraints {
    const fn empty() -> Self {
        Self {
            minimum: None,
            maximum: None,
            min_length: None,
            max_length: None,
            min_items: None,
            max_items: None,
            max_properties: None,
            pattern: None,
            format: None,
            content_encoding: None,
            trimmed_non_empty: false,
        }
    }

    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        minimum: Option<f64>,
        maximum: Option<f64>,
        min_length: Option<usize>,
        max_length: Option<usize>,
        min_items: Option<usize>,
        max_items: Option<usize>,
        max_properties: Option<usize>,
        pattern: Option<StringPattern>,
        format: Option<StringFormat>,
        content_encoding: Option<ContentEncoding>,
        trimmed_non_empty: bool,
    ) -> Self {
        Self {
            minimum,
            maximum,
            min_length,
            max_length,
            min_items,
            max_items,
            max_properties,
            pattern,
            format,
            content_encoding,
            trimmed_non_empty,
        }
    }
}

impl Schema {
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        value_types: &'static [Type],
        required: &'static [&'static str],
        properties: &'static [Property],
        additional_properties: bool,
        allowed_values: &'static [Literal],
        constant: Option<Literal>,
        alternatives: &'static [Schema],
        items: Option<&'static Schema>,
        constraints: Constraints,
    ) -> Self {
        Self {
            reference: None,
            value_types,
            required,
            properties,
            additional_properties,
            allowed_values,
            constant,
            alternatives,
            items,
            constraints,
        }
    }

    #[doc(hidden)]
    pub const fn reference(index: usize) -> Self {
        Self {
            reference: Some(index),
            value_types: &[],
            required: &[],
            properties: &[],
            additional_properties: true,
            allowed_values: &[],
            constant: None,
            alternatives: &[],
            items: None,
            constraints: Constraints::empty(),
        }
    }

    fn validate_raw(
        &self,
        raw: &RawValue,
        definitions: &'static [Schema],
    ) -> Result<(), ValidationError> {
        self.validate_raw_mode(raw, definitions, true)
    }

    fn validate_raw_mode(
        &self,
        raw: &RawValue,
        definitions: &'static [Schema],
        track_path: bool,
    ) -> Result<(), ValidationError> {
        if let Some(index) = self.reference {
            return definitions
                .get(index)
                .ok_or_else(|| ValidationError::new(ErrorKind::InvalidJson))?
                .validate_raw_mode(raw, definitions, track_path);
        }
        if !self.alternatives.is_empty() {
            let matches = self
                .alternatives
                .iter()
                .filter(|schema| schema.validate_raw_mode(raw, definitions, false).is_ok())
                .count();
            if matches != 1 {
                return Err(ValidationError::new(ErrorKind::OneOf));
            }
        }

        let raw_type = raw_type(raw.get());
        if !self.value_types.is_empty()
            && !self
                .value_types
                .iter()
                .any(|expected| raw_matches_type(raw.get(), raw_type, *expected))
        {
            let expected = self
                .value_types
                .first()
                .copied()
                .ok_or_else(|| ValidationError::new(ErrorKind::InvalidJson))?;
            return Err(ValidationError::new(ErrorKind::Type { expected }));
        }
        if !self.allowed_values.is_empty()
            && !self
                .allowed_values
                .iter()
                .any(|allowed| allowed.matches_raw(raw))
        {
            return Err(ValidationError::new(ErrorKind::Enum));
        }
        if self
            .constant
            .is_some_and(|constant| !constant.matches_raw(raw))
        {
            return Err(ValidationError::new(ErrorKind::Const));
        }

        match raw_type {
            RawType::Object => self.validate_raw_object(raw, definitions, track_path)?,
            RawType::Array => self.validate_raw_array(raw, definitions, track_path)?,
            RawType::String => {
                let string = decode_string(raw.get())?;
                self.validate_string(&string)?;
            }
            RawType::Number => {
                let number = serde_json::from_str::<serde_json::Number>(raw.get())
                    .map_err(|_| ValidationError::new(ErrorKind::InvalidJson))?;
                self.validate_number(
                    number
                        .as_f64()
                        .ok_or_else(|| ValidationError::new(ErrorKind::InvalidJson))?,
                )?;
            }
            RawType::Boolean | RawType::Null => {}
        }
        Ok(())
    }

    fn validate_raw_object(
        &self,
        raw: &RawValue,
        definitions: &'static [Schema],
        track_path: bool,
    ) -> Result<(), ValidationError> {
        let mut state = ObjectState::default();
        let mut nested_error = None;
        let visitor = ObjectVisitor {
            schema: self,
            definitions,
            track_path,
            state: &mut state,
            nested_error: &mut nested_error,
        };
        let result = serde_json::Deserializer::from_str(raw.get()).deserialize_map(visitor);
        if let Some(error) = nested_error {
            return Err(error);
        }
        result.map_err(|_| ValidationError::new(ErrorKind::InvalidJson))?;

        if self
            .constraints
            .max_properties
            .is_some_and(|maximum| state.count > maximum)
        {
            return Err(ValidationError::new(ErrorKind::MaxProperties));
        }
        for (index, required) in self.required.iter().enumerate() {
            let mask = required_mask(index)?;
            if state.required & mask == 0 {
                return Err(ValidationError::new(ErrorKind::Required)
                    .prepend_property_if(required, track_path));
            }
        }
        Ok(())
    }

    fn validate_raw_array(
        &self,
        raw: &RawValue,
        definitions: &'static [Schema],
        track_path: bool,
    ) -> Result<(), ValidationError> {
        let mut count = 0_usize;
        let mut nested_error = None;
        let visitor = ArrayVisitor {
            items: self.items,
            definitions,
            track_path,
            count: &mut count,
            nested_error: &mut nested_error,
        };
        let result = serde_json::Deserializer::from_str(raw.get()).deserialize_seq(visitor);
        if let Some(error) = nested_error {
            return Err(error);
        }
        result.map_err(|_| ValidationError::new(ErrorKind::InvalidJson))?;
        if self
            .constraints
            .min_items
            .is_some_and(|minimum| count < minimum)
        {
            return Err(ValidationError::new(ErrorKind::MinItems));
        }
        if self
            .constraints
            .max_items
            .is_some_and(|maximum| count > maximum)
        {
            return Err(ValidationError::new(ErrorKind::MaxItems));
        }
        Ok(())
    }

    fn validate_value(
        &self,
        value: &Value,
        definitions: &'static [Schema],
    ) -> Result<(), ValidationError> {
        if let Some(index) = self.reference {
            return definitions
                .get(index)
                .ok_or_else(|| ValidationError::new(ErrorKind::InvalidJson))?
                .validate_value(value, definitions);
        }
        if !self.alternatives.is_empty() {
            let matches = self
                .alternatives
                .iter()
                .filter(|schema| schema.validate_value(value, definitions).is_ok())
                .count();
            if matches != 1 {
                return Err(ValidationError::new(ErrorKind::OneOf));
            }
        }
        if !self.value_types.is_empty()
            && !self
                .value_types
                .iter()
                .any(|expected| matches_type(value, *expected))
        {
            let expected = self
                .value_types
                .first()
                .copied()
                .ok_or_else(|| ValidationError::new(ErrorKind::InvalidJson))?;
            return Err(ValidationError::new(ErrorKind::Type { expected }));
        }
        if !self.allowed_values.is_empty()
            && !self
                .allowed_values
                .iter()
                .any(|allowed| allowed.matches_value(value))
        {
            return Err(ValidationError::new(ErrorKind::Enum));
        }
        if self
            .constant
            .is_some_and(|constant| !constant.matches_value(value))
        {
            return Err(ValidationError::new(ErrorKind::Const));
        }
        if let Some(string) = value.as_str() {
            self.validate_string(string)?;
        }
        if let Some(number) = value.as_f64() {
            self.validate_number(number)?;
        }
        if let Some(object) = value.as_object() {
            if self
                .constraints
                .max_properties
                .is_some_and(|maximum| object.len() > maximum)
            {
                return Err(ValidationError::new(ErrorKind::MaxProperties));
            }
            for field in self.required {
                if !object.contains_key(*field) {
                    return Err(ValidationError::new(ErrorKind::Required).prepend_property(field));
                }
            }
            if !self.additional_properties {
                for field in object.keys() {
                    if !self
                        .properties
                        .iter()
                        .any(|property| property.name == field)
                    {
                        return Err(ValidationError::new(ErrorKind::AdditionalProperty)
                            .prepend_property(field));
                    }
                }
            }
            for property in self.properties {
                if let Some(field_value) = object.get(property.name) {
                    property
                        .schema
                        .validate_value(field_value, definitions)
                        .map_err(|error| error.prepend_property(property.name))?;
                }
            }
        }
        if let Some(values) = value.as_array() {
            if self
                .constraints
                .min_items
                .is_some_and(|minimum| values.len() < minimum)
            {
                return Err(ValidationError::new(ErrorKind::MinItems));
            }
            if self
                .constraints
                .max_items
                .is_some_and(|maximum| values.len() > maximum)
            {
                return Err(ValidationError::new(ErrorKind::MaxItems));
            }
            if let Some(items) = self.items {
                for (index, item) in values.iter().enumerate() {
                    items
                        .validate_value(item, definitions)
                        .map_err(|error| error.prepend_index(index))?;
                }
            }
        }
        Ok(())
    }

    fn validate_string(&self, string: &str) -> Result<(), ValidationError> {
        let length = string.chars().count();
        if self
            .constraints
            .min_length
            .is_some_and(|minimum| length < minimum)
        {
            return Err(ValidationError::new(ErrorKind::MinLength));
        }
        if self
            .constraints
            .max_length
            .is_some_and(|maximum| length > maximum)
        {
            return Err(ValidationError::new(ErrorKind::MaxLength));
        }
        if self.constraints.trimmed_non_empty && string.trim().is_empty() {
            return Err(ValidationError::new(ErrorKind::TrimmedEmpty));
        }
        if self
            .constraints
            .pattern
            .is_some_and(|pattern| !pattern.matches(string))
        {
            return Err(ValidationError::new(ErrorKind::Pattern));
        }
        if self
            .constraints
            .format
            .is_some_and(|format| !format.matches(string))
        {
            return Err(ValidationError::new(ErrorKind::Format));
        }
        if self
            .constraints
            .content_encoding
            .is_some_and(|encoding| !encoding.matches(string))
        {
            return Err(ValidationError::new(ErrorKind::ContentEncoding));
        }
        Ok(())
    }

    fn validate_number(&self, number: f64) -> Result<(), ValidationError> {
        if self
            .constraints
            .minimum
            .is_some_and(|minimum| number < minimum)
        {
            return Err(ValidationError::new(ErrorKind::Minimum));
        }
        if self
            .constraints
            .maximum
            .is_some_and(|maximum| number > maximum)
        {
            return Err(ValidationError::new(ErrorKind::Maximum));
        }
        Ok(())
    }
}

#[derive(Default)]
struct ObjectState {
    required: u128,
    count: usize,
}

struct ObjectVisitor<'schema, 'state> {
    schema: &'schema Schema,
    definitions: &'static [Schema],
    track_path: bool,
    state: &'state mut ObjectState,
    nested_error: &'state mut Option<ValidationError>,
}

impl<'de> Visitor<'de> for ObjectVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        while let Some(field) = map.next_key::<BorrowedString<'de>>()? {
            let field = field.0;
            self.state.count = self
                .state
                .count
                .checked_add(1)
                .ok_or_else(|| A::Error::custom("property count overflow"))?;
            if let Some(index) = self
                .schema
                .required
                .iter()
                .position(|name| *name == field.as_ref())
            {
                let mask = required_mask(index).map_err(A::Error::custom)?;
                self.state.required |= mask;
            }
            let value = map.next_value::<&'de RawValue>()?;
            if self.nested_error.is_some() {
                continue;
            }
            if let Some(property) = self
                .schema
                .properties
                .iter()
                .find(|property| property.name == field.as_ref())
            {
                if let Err(error) =
                    property
                        .schema
                        .validate_raw_mode(value, self.definitions, self.track_path)
                {
                    *self.nested_error = Some(error.prepend_property_if(&field, self.track_path));
                }
            } else if !self.schema.additional_properties {
                *self.nested_error = Some(
                    ValidationError::new(ErrorKind::AdditionalProperty)
                        .prepend_property_if(&field, self.track_path),
                );
            }
        }
        Ok(())
    }
}

struct ArrayVisitor<'schema, 'state> {
    items: Option<&'schema Schema>,
    definitions: &'static [Schema],
    track_path: bool,
    count: &'state mut usize,
    nested_error: &'state mut Option<ValidationError>,
}

impl<'de> Visitor<'de> for ArrayVisitor<'_, '_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(value) = sequence.next_element::<&'de RawValue>()? {
            let index = *self.count;
            *self.count = self
                .count
                .checked_add(1)
                .ok_or_else(|| A::Error::custom("item count overflow"))?;
            if self.nested_error.is_some() {
                continue;
            }
            if let Some(items) = self.items {
                if let Err(error) =
                    items.validate_raw_mode(value, self.definitions, self.track_path)
                {
                    *self.nested_error = Some(error.prepend_index_if(index, self.track_path));
                }
            }
        }
        Ok(())
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Property {
    name: &'static str,
    schema: Schema,
}

impl Property {
    #[doc(hidden)]
    pub const fn new(name: &'static str, schema: Schema) -> Self {
        Self { name, schema }
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Literal {
    String(&'static str),
    Number(f64),
    Boolean(bool),
    Null,
}

impl Literal {
    fn matches_raw(self, raw: &RawValue) -> bool {
        match self {
            Self::String(expected) => {
                decode_string(raw.get()).is_ok_and(|actual| actual == expected)
            }
            Self::Number(expected) => {
                serde_json::from_str::<f64>(raw.get()).is_ok_and(|actual| actual == expected)
            }
            Self::Boolean(expected) => {
                serde_json::from_str::<bool>(raw.get()).is_ok_and(|actual| actual == expected)
            }
            Self::Null => raw.get().trim() == "null",
        }
    }

    fn matches_value(self, value: &Value) -> bool {
        match self {
            Self::String(expected) => value.as_str() == Some(expected),
            Self::Number(expected) => value.as_f64() == Some(expected),
            Self::Boolean(expected) => value.as_bool() == Some(expected),
            Self::Null => value.is_null(),
        }
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringPattern {
    AsciiIdentifier,
    HttpUrl,
    SessionId,
    RunId,
    InputId,
    CommandId,
    UtcMilliseconds,
}

impl StringPattern {
    fn matches(self, value: &str) -> bool {
        match self {
            Self::AsciiIdentifier => {
                !value.is_empty()
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
            }
            Self::HttpUrl => value.starts_with("http://") || value.starts_with("https://"),
            Self::SessionId => prefixed_decimal(value, "session-"),
            Self::RunId => prefixed_decimal(value, "run-"),
            Self::InputId => prefixed_decimal(value, "input-"),
            Self::CommandId => prefixed_decimal(value, "command-"),
            Self::UtcMilliseconds => is_utc_milliseconds(value),
        }
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringFormat {
    DateTime,
}

impl StringFormat {
    fn matches(self, value: &str) -> bool {
        match self {
            Self::DateTime => is_rfc3339(value),
        }
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentEncoding {
    Base64,
}

impl ContentEncoding {
    fn matches(self, value: &str) -> bool {
        match self {
            Self::Base64 => is_base64(value),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    InvalidJson,
    Type { expected: Type },
    Required,
    AdditionalProperty,
    Enum,
    Const,
    OneOf,
    Minimum,
    Maximum,
    MinLength,
    MaxLength,
    MinItems,
    MaxItems,
    MaxProperties,
    Pattern,
    Format,
    ContentEncoding,
    TrimmedEmpty,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("JSON schema validation failed at {path}: {kind:?}")]
pub struct ValidationError {
    pub path: String,
    pub kind: ErrorKind,
}

impl ValidationError {
    fn new(kind: ErrorKind) -> Self {
        Self {
            path: String::new(),
            kind,
        }
    }

    fn prepend_property(mut self, field: &str) -> Self {
        self.path = format!(".{field}{}", self.path);
        self
    }

    fn prepend_property_if(self, field: &str, track_path: bool) -> Self {
        if track_path {
            self.prepend_property(field)
        } else {
            self
        }
    }

    fn prepend_index(mut self, index: usize) -> Self {
        self.path = format!("[{index}]{}", self.path);
        self
    }

    fn prepend_index_if(self, index: usize, track_path: bool) -> Self {
        if track_path {
            self.prepend_index(index)
        } else {
            self
        }
    }

    fn with_root(mut self) -> Self {
        self.path = format!("${}", self.path);
        self
    }
}

#[derive(Clone, Copy)]
enum RawType {
    Object,
    Array,
    String,
    Number,
    Boolean,
    Null,
}

struct BorrowedString<'a>(Cow<'a, str>);

impl<'de> Deserialize<'de> for BorrowedString<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct StringVisitor;

        impl<'de> Visitor<'de> for StringVisitor {
            type Value = BorrowedString<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON string")
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(BorrowedString(Cow::Borrowed(value)))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(BorrowedString(Cow::Owned(value.into())))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(BorrowedString(Cow::Owned(value)))
            }
        }

        deserializer.deserialize_str(StringVisitor)
    }
}

fn decode_string(raw: &str) -> Result<Cow<'_, str>, ValidationError> {
    if let Ok(value) = serde_json::from_str::<&str>(raw) {
        return Ok(Cow::Borrowed(value));
    }
    serde_json::from_str::<String>(raw)
        .map(Cow::Owned)
        .map_err(|_| ValidationError::new(ErrorKind::InvalidJson))
}

fn raw_type(raw: &str) -> RawType {
    match raw.trim_start().as_bytes().first().copied() {
        Some(b'{') => RawType::Object,
        Some(b'[') => RawType::Array,
        Some(b'"') => RawType::String,
        Some(b't' | b'f') => RawType::Boolean,
        Some(b'n') => RawType::Null,
        _ => RawType::Number,
    }
}

fn raw_matches_type(raw: &str, actual: RawType, expected: Type) -> bool {
    match expected {
        Type::Object => matches!(actual, RawType::Object),
        Type::Array => matches!(actual, RawType::Array),
        Type::String => matches!(actual, RawType::String),
        Type::Integer => {
            matches!(actual, RawType::Number)
                && !raw.as_bytes().contains(&b'.')
                && !raw.as_bytes().contains(&b'e')
                && !raw.as_bytes().contains(&b'E')
        }
        Type::Number => matches!(actual, RawType::Number),
        Type::Boolean => matches!(actual, RawType::Boolean),
        Type::Null => matches!(actual, RawType::Null),
    }
}

fn matches_type(value: &Value, expected: Type) -> bool {
    match expected {
        Type::Object => value.is_object(),
        Type::Array => value.is_array(),
        Type::String => value.is_string(),
        Type::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        Type::Number => value.is_number(),
        Type::Boolean => value.is_boolean(),
        Type::Null => value.is_null(),
    }
}

fn required_mask(index: usize) -> Result<u128, ValidationError> {
    let shift = u32::try_from(index).map_err(|_| ValidationError::new(ErrorKind::InvalidJson))?;
    1_u128
        .checked_shl(shift)
        .ok_or_else(|| ValidationError::new(ErrorKind::InvalidJson))
}

fn prefixed_decimal(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn is_base64(value: &str) -> bool {
    if !value.len().is_multiple_of(4) {
        return false;
    }
    let padding = value.bytes().rev().take_while(|byte| *byte == b'=').count();
    if padding > 2 {
        return false;
    }
    let data_length = value.len().saturating_sub(padding);
    value
        .bytes()
        .take(data_length)
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
        && value.bytes().skip(data_length).all(|byte| byte == b'=')
}

fn is_utc_milliseconds(value: &str) -> bool {
    value.len() == 24
        && value.as_bytes().get(19) == Some(&b'.')
        && value.as_bytes().get(23) == Some(&b'Z')
        && value
            .as_bytes()
            .get(20..23)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_digit))
        && valid_date_time_prefix(value.as_bytes().get(..19).unwrap_or_default())
}

fn is_rfc3339(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 20 || !valid_date_time_prefix(bytes.get(..19).unwrap_or_default()) {
        return false;
    }
    let mut index = 19_usize;
    if bytes.get(index) == Some(&b'.') {
        index = index.saturating_add(1);
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index = index.saturating_add(1);
        }
        if index == start {
            return false;
        }
    }
    if bytes.get(index) == Some(&b'Z') {
        return index.saturating_add(1) == bytes.len();
    }
    if !matches!(bytes.get(index), Some(b'+' | b'-')) {
        return false;
    }
    let zone = bytes.get(index.saturating_add(1)..).unwrap_or_default();
    zone.len() == 5
        && zone.get(2) == Some(&b':')
        && decimal_pair(zone.get(..2).unwrap_or_default()).is_some_and(|hour| hour <= 23)
        && decimal_pair(zone.get(3..).unwrap_or_default()).is_some_and(|minute| minute <= 59)
}

fn valid_date_time_prefix(bytes: &[u8]) -> bool {
    bytes.len() == 19
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b'T')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':')
        && decimal_quad(bytes.get(..4).unwrap_or_default()).is_some()
        && decimal_pair(bytes.get(5..7).unwrap_or_default())
            .is_some_and(|month| (1..=12).contains(&month))
        && decimal_pair(bytes.get(8..10).unwrap_or_default())
            .is_some_and(|day| (1..=31).contains(&day))
        && decimal_pair(bytes.get(11..13).unwrap_or_default()).is_some_and(|hour| hour <= 23)
        && decimal_pair(bytes.get(14..16).unwrap_or_default()).is_some_and(|minute| minute <= 59)
        && decimal_pair(bytes.get(17..19).unwrap_or_default()).is_some_and(|second| second <= 59)
}

fn decimal_pair(bytes: &[u8]) -> Option<u8> {
    let [tens, ones] = bytes else {
        return None;
    };
    if !tens.is_ascii_digit() || !ones.is_ascii_digit() {
        return None;
    }
    tens.checked_sub(b'0')?
        .checked_mul(10)?
        .checked_add(ones.checked_sub(b'0')?)
}

fn decimal_quad(bytes: &[u8]) -> Option<u16> {
    let [a, b, c, d] = bytes else {
        return None;
    };
    if !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    u16::from(a.checked_sub(b'0')?)
        .checked_mul(1000)?
        .checked_add(u16::from(b.checked_sub(b'0')?).checked_mul(100)?)?
        .checked_add(u16::from(c.checked_sub(b'0')?).checked_mul(10)?)?
        .checked_add(u16::from(d.checked_sub(b'0')?))
}
