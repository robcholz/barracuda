//! `no_std + alloc` validation for a deliberately small, statically compiled
//! subset of JSON Schema used by agent tool arguments.
//!
//! [`validator!`] reads a schema file during compilation. Unsupported keywords
//! are compile errors rather than silently ignored constraints. Validation at
//! runtime walks only static rules and the supplied [`serde_json::Value`].

#![no_std]

extern crate alloc;

use alloc::{format, string::String};

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
    Minimum,
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
