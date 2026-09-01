//! `no_std + alloc` validation for a deliberately small, statically compiled
//! subset of JSON Schema used by agent tool arguments.
//!
//! [`validator!`] reads a schema file during compilation. Unsupported keywords
//! are compile errors rather than silently ignored constraints. Validation at
//! runtime walks only static rules and the supplied [`serde_json::Value`].

#![no_std]

extern crate alloc;

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};

use serde_json::Value;

pub use json_validator_macros::validator;

#[derive(Clone, Copy, Debug)]
pub struct Validator {
    schema: Schema,
}

impl Validator {
    #[doc(hidden)]
    pub const fn from_schema(schema: Schema) -> Self {
        Self { schema }
    }

    pub fn validate(&self, value: &Value) -> Result<(), ValidationError> {
        self.schema
            .validate(value)
            .map_err(ValidationError::with_root)
    }
}

/// Runtime-compiled validator for a baked JSON Schema document.
///
/// Compilation rejects unsupported assertion keywords instead of silently
/// accepting a contract this crate cannot enforce.
#[derive(Clone, Debug)]
pub struct OwnedValidator {
    schema: OwnedSchema,
}

impl OwnedValidator {
    /// Compiles one JSON Schema document into an allocation-owned validator.
    ///
    /// # Errors
    ///
    /// Returns an error when the document is invalid JSON or uses unsupported
    /// schema assertions.
    pub fn from_json(document: &str) -> Result<Self, OwnedValidatorError> {
        let value: Value = serde_json::from_str(document)
            .map_err(|error| OwnedValidatorError::new(error.to_string()))?;
        Ok(Self {
            schema: OwnedSchema::compile(&value, "$schema")?,
        })
    }

    /// Validates a JSON value against the compiled schema.
    ///
    /// # Errors
    ///
    /// Returns the first failed assertion with its JSON path.
    pub fn validate(&self, value: &Value) -> Result<(), ValidationError> {
        self.schema
            .validate(value)
            .map_err(ValidationError::with_root)
    }
}

/// Failure while compiling an owned JSON Schema validator.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OwnedValidatorError {
    message: String,
}

impl OwnedValidatorError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
struct OwnedSchema {
    value_type: Option<Type>,
    required: Vec<String>,
    properties: BTreeMap<String, OwnedSchema>,
    additional_properties: bool,
    allowed_values: Vec<Value>,
    items: Option<Box<OwnedSchema>>,
    one_of: Vec<OwnedSchema>,
    minimum: Option<f64>,
    min_length: Option<usize>,
    max_length: Option<usize>,
    min_items: Option<usize>,
    max_items: Option<usize>,
}

impl OwnedSchema {
    fn compile(value: &Value, path: &str) -> Result<Self, OwnedValidatorError> {
        let object = value
            .as_object()
            .ok_or_else(|| OwnedValidatorError::new(format!("{path} must be a JSON object")))?;
        for key in object.keys() {
            if !matches!(
                key.as_str(),
                "type"
                    | "properties"
                    | "required"
                    | "additionalProperties"
                    | "enum"
                    | "items"
                    | "oneOf"
                    | "minimum"
                    | "minLength"
                    | "maxLength"
                    | "minItems"
                    | "maxItems"
                    | "title"
                    | "description"
                    | "format"
                    | "default"
                    | "examples"
                    | "deprecated"
                    | "readOnly"
                    | "writeOnly"
                    | "$schema"
            ) {
                return Err(OwnedValidatorError::new(format!(
                    "{path}.{key} is not supported by json-validator"
                )));
            }
        }

        let value_type = match object.get("type") {
            Some(Value::String(value_type)) => Some(parse_type(value_type, path)?),
            Some(_) => {
                return Err(OwnedValidatorError::new(format!(
                    "{path}.type must be a string"
                )))
            }
            None => None,
        };
        let required = string_array(object.get("required"), &format!("{path}.required"))?;
        let mut properties = BTreeMap::new();
        if let Some(value) = object.get("properties") {
            let fields = value.as_object().ok_or_else(|| {
                OwnedValidatorError::new(format!("{path}.properties must be an object"))
            })?;
            for (name, field_schema) in fields {
                properties.insert(
                    name.clone(),
                    Self::compile(field_schema, &format!("{path}.properties.{name}"))?,
                );
            }
        }
        let additional_properties = match object.get("additionalProperties") {
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(OwnedValidatorError::new(format!(
                    "{path}.additionalProperties must be boolean"
                )))
            }
            None => true,
        };
        let allowed_values = match object.get("enum") {
            Some(Value::Array(values)) if !values.is_empty() => values.clone(),
            Some(Value::Array(_)) => {
                return Err(OwnedValidatorError::new(format!(
                    "{path}.enum must not be empty"
                )))
            }
            Some(_) => {
                return Err(OwnedValidatorError::new(format!(
                    "{path}.enum must be an array"
                )))
            }
            None => Vec::new(),
        };
        let items = object
            .get("items")
            .map(|items| Self::compile(items, &format!("{path}.items")).map(Box::new))
            .transpose()?;
        let one_of = match object.get("oneOf") {
            Some(Value::Array(variants)) if !variants.is_empty() => variants
                .iter()
                .enumerate()
                .map(|(index, variant)| Self::compile(variant, &format!("{path}.oneOf[{index}]")))
                .collect::<Result<Vec<_>, _>>()?,
            Some(Value::Array(_)) => {
                return Err(OwnedValidatorError::new(format!(
                    "{path}.oneOf must not be empty"
                )))
            }
            Some(_) => {
                return Err(OwnedValidatorError::new(format!(
                    "{path}.oneOf must be an array"
                )))
            }
            None => Vec::new(),
        };

        Ok(Self {
            value_type,
            required,
            properties,
            additional_properties,
            allowed_values,
            items,
            one_of,
            minimum: optional_number(object.get("minimum"), &format!("{path}.minimum"))?,
            min_length: optional_usize(object.get("minLength"), &format!("{path}.minLength"))?,
            max_length: optional_usize(object.get("maxLength"), &format!("{path}.maxLength"))?,
            min_items: optional_usize(object.get("minItems"), &format!("{path}.minItems"))?,
            max_items: optional_usize(object.get("maxItems"), &format!("{path}.maxItems"))?,
        })
    }

    fn validate(&self, value: &Value) -> Result<(), ValidationError> {
        if let Some(expected) = self.value_type {
            if !matches_type(value, expected) {
                return Err(ValidationError::new(ErrorKind::Type { expected }));
            }
        }
        if !self.allowed_values.is_empty() && !self.allowed_values.contains(value) {
            return Err(ValidationError::new(ErrorKind::Enum));
        }
        if !self.one_of.is_empty() {
            let matches = self
                .one_of
                .iter()
                .filter(|schema| schema.validate(value).is_ok())
                .count();
            if matches != 1 {
                return Err(ValidationError::new(ErrorKind::OneOf));
            }
        }
        if let Some(object) = value.as_object() {
            for field in &self.required {
                if !object.contains_key(field) {
                    return Err(ValidationError::new(ErrorKind::Required).prepend_property(field));
                }
            }
            if !self.additional_properties {
                for field in object.keys() {
                    if !self.properties.contains_key(field) {
                        return Err(ValidationError::new(ErrorKind::AdditionalProperty)
                            .prepend_property(field));
                    }
                }
            }
            for (name, schema) in &self.properties {
                if let Some(field_value) = object.get(name) {
                    schema
                        .validate(field_value)
                        .map_err(|error| error.prepend_property(name))?;
                }
            }
        }
        if let Some(values) = value.as_array() {
            if self.min_items.is_some_and(|minimum| values.len() < minimum) {
                return Err(ValidationError::new(ErrorKind::MinItems));
            }
            if self.maximum_items_exceeded(values.len()) {
                return Err(ValidationError::new(ErrorKind::MaxItems));
            }
            if let Some(items) = &self.items {
                for (index, item) in values.iter().enumerate() {
                    items
                        .validate(item)
                        .map_err(|error| error.prepend_index(index))?;
                }
            }
        }
        if let Some(string) = value.as_str() {
            let length = string.chars().count();
            if self.min_length.is_some_and(|minimum| length < minimum) {
                return Err(ValidationError::new(ErrorKind::MinLength));
            }
            if self.max_length.is_some_and(|maximum| length > maximum) {
                return Err(ValidationError::new(ErrorKind::MaxLength));
            }
        }
        if let (Some(minimum), Some(number)) = (self.minimum, value.as_f64()) {
            if number < minimum {
                return Err(ValidationError::new(ErrorKind::Minimum));
            }
        }
        Ok(())
    }

    fn maximum_items_exceeded(&self, length: usize) -> bool {
        self.max_items.is_some_and(|maximum| length > maximum)
    }
}

fn parse_type(value: &str, path: &str) -> Result<Type, OwnedValidatorError> {
    match value {
        "object" => Ok(Type::Object),
        "array" => Ok(Type::Array),
        "string" => Ok(Type::String),
        "integer" => Ok(Type::Integer),
        "number" => Ok(Type::Number),
        "boolean" => Ok(Type::Boolean),
        "null" => Ok(Type::Null),
        other => Err(OwnedValidatorError::new(format!(
            "{path}.type '{other}' is unsupported"
        ))),
    }
}

fn string_array(value: Option<&Value>, path: &str) -> Result<Vec<String>, OwnedValidatorError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| OwnedValidatorError::new(format!("{path} must be an array")))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .ok_or_else(|| OwnedValidatorError::new(format!("{path} entries must be strings")))
        })
        .collect()
}

fn optional_number(value: Option<&Value>, path: &str) -> Result<Option<f64>, OwnedValidatorError> {
    value
        .map(|value| {
            value
                .as_f64()
                .ok_or_else(|| OwnedValidatorError::new(format!("{path} must be a number")))
        })
        .transpose()
}

fn optional_usize(value: Option<&Value>, path: &str) -> Result<Option<usize>, OwnedValidatorError> {
    value
        .map(|value| {
            let raw = value.as_u64().ok_or_else(|| {
                OwnedValidatorError::new(format!("{path} must be a non-negative integer"))
            })?;
            usize::try_from(raw).map_err(|_| {
                OwnedValidatorError::new(format!("{path} exceeds the platform size limit"))
            })
        })
        .transpose()
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
#[derive(Clone, Copy, Debug)]
pub struct Schema {
    value_type: Option<Type>,
    required: &'static [&'static str],
    properties: &'static [Property],
    additional_properties: bool,
    allowed_values: &'static [Literal],
    items: Option<&'static Schema>,
    constraints: Constraints,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct Constraints {
    minimum: Option<f64>,
    trimmed_non_empty: bool,
}

impl Constraints {
    #[doc(hidden)]
    pub const fn new(minimum: Option<f64>, trimmed_non_empty: bool) -> Self {
        Self {
            minimum,
            trimmed_non_empty,
        }
    }
}

impl Schema {
    #[doc(hidden)]
    pub const fn new(
        value_type: Option<Type>,
        required: &'static [&'static str],
        properties: &'static [Property],
        additional_properties: bool,
        allowed_values: &'static [Literal],
        items: Option<&'static Schema>,
        constraints: Constraints,
    ) -> Self {
        Self {
            value_type,
            required,
            properties,
            additional_properties,
            allowed_values,
            items,
            constraints,
        }
    }

    fn validate(&self, value: &Value) -> Result<(), ValidationError> {
        if let Some(expected) = self.value_type {
            if !matches_type(value, expected) {
                return Err(ValidationError::new(ErrorKind::Type { expected }));
            }
        }

        if !self.allowed_values.is_empty()
            && !self
                .allowed_values
                .iter()
                .any(|allowed| allowed.matches(value))
        {
            return Err(ValidationError::new(ErrorKind::Enum));
        }

        if self.constraints.trimmed_non_empty
            && value
                .as_str()
                .is_some_and(|string| string.trim().is_empty())
        {
            return Err(ValidationError::new(ErrorKind::TrimmedEmpty));
        }

        if let Some(object) = value.as_object() {
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
                        .validate(field_value)
                        .map_err(|error| error.prepend_property(property.name))?;
                }
            }
        }

        if let (Some(items), Some(values)) = (self.items, value.as_array()) {
            for (index, item) in values.iter().enumerate() {
                items
                    .validate(item)
                    .map_err(|error| error.prepend_index(index))?;
            }
        }

        if let (Some(minimum), Some(number)) = (self.constraints.minimum, value.as_f64()) {
            if number < minimum {
                return Err(ValidationError::new(ErrorKind::Minimum));
            }
        }

        Ok(())
    }
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
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
#[derive(Clone, Copy, Debug)]
pub enum Literal {
    String(&'static str),
}

impl Literal {
    fn matches(self, value: &Value) -> bool {
        match self {
            Self::String(expected) => value.as_str() == Some(expected),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    Type { expected: Type },
    Required,
    AdditionalProperty,
    Enum,
    OneOf,
    Minimum,
    MinLength,
    MaxLength,
    MinItems,
    MaxItems,
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

    fn prepend_index(mut self, index: usize) -> Self {
        self.path = format!("[{index}]{}", self.path);
        self
    }

    fn with_root(mut self) -> Self {
        self.path = format!("${}", self.path);
        self
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

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use serde_json::json;

    const SCHEMA: &str = r#"{
        "type":"object",
        "required":["method","headers"],
        "properties":{
            "method":{"oneOf":[{"type":"string","enum":["GET"]},{"type":"string","enum":["POST"]}]},
            "headers":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"string","maxLength":4}}
        },
        "additionalProperties":false
    }"#;

    #[test]
    fn owned_validator_enforces_baked_object_constraints() {
        let validator = OwnedValidator::from_json(SCHEMA).expect("compile schema");

        assert!(validator
            .validate(&json!({"method":"GET", "headers":["name"]}))
            .is_ok());
        assert_eq!(
            validator
                .validate(&json!({"method":"GET", "headers":[], "extra":true}))
                .map_err(|error| error.kind),
            Err(ErrorKind::AdditionalProperty)
        );
        assert_eq!(
            validator
                .validate(&json!({"method":"PATCH", "headers":["name"]}))
                .map_err(|error| error.kind),
            Err(ErrorKind::OneOf)
        );
        assert_eq!(
            validator
                .validate(&json!({"method":"GET", "headers":["longer"]}))
                .map_err(|error| error.kind),
            Err(ErrorKind::MaxLength)
        );
    }

    #[test]
    fn owned_validator_rejects_unknown_assertions() {
        let result = OwnedValidator::from_json(r#"{"type":"string","pattern":"x"}"#);
        assert!(matches!(result, Err(error) if error.to_string().contains("pattern")));
    }
}
