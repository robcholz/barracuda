//! Values bound into a view and the types that describe them.

use alloc::{boxed::Box, rc::Rc, string::String, vec::Vec};
use core::fmt::Write;
use microts::display;

/// A value read by view bindings or passed to a handler.
#[derive(Clone, PartialEq, Debug)]
pub enum Value {
    /// No value; bindings that fail to evaluate also produce this.
    Void,
    /// A boolean.
    Bool(bool),
    /// A 32-bit integer (`i32` in the view contract).
    I32(i32),
    /// A 32-bit float (`f32` and `Px` in the view contract).
    F32(f32),
    /// A 64-bit float (`f64` and plain `number` in the view contract).
    F64(f64),
    /// A string.
    Str(Rc<str>),
    /// A struct; fields are in the declaration order of its [`StructType`].
    Struct(Rc<[Value]>),
    /// An array of values of one element type.
    Array(Rc<[Value]>),
}

impl Value {
    pub(crate) fn truthy(&self) -> bool {
        match self {
            Value::Bool(value) => *value,
            Value::I32(value) => *value != 0,
            Value::F32(value) => *value != 0.0,
            Value::F64(value) => *value != 0.0,
            Value::Str(value) => !value.is_empty(),
            Value::Struct(_) | Value::Array(_) => true,
            Value::Void => false,
        }
    }

    pub(crate) fn as_f64(&self) -> f64 {
        match self {
            Value::I32(value) => f64::from(*value),
            Value::F32(value) => f64::from(*value),
            Value::F64(value) => *value,
            _ => 0.0,
        }
    }

    pub(crate) fn format_into(&self, out: &mut String) {
        let _ = match self {
            Value::I32(value) => write!(out, "{}", display(value)),
            Value::F32(value) => write!(out, "{}", display(value)),
            Value::F64(value) => write!(out, "{}", display(value)),
            Value::Bool(value) => write!(out, "{}", display(value)),
            Value::Str(value) => out.write_str(value),
            Value::Void | Value::Struct(_) | Value::Array(_) => Ok(()),
        };
    }
}

/// The identity of a row in a keyed list.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Key {
    /// A row whose key expression did not produce a string, integer or boolean.
    Other,
    /// A boolean key.
    Bool(bool),
    /// An integer key.
    Int(i32),
    /// A string key.
    Str(Rc<str>),
}

impl From<&Value> for Key {
    fn from(value: &Value) -> Self {
        match value {
            Value::Bool(value) => Key::Bool(*value),
            Value::I32(value) => Key::Int(*value),
            Value::Str(value) => Key::Str(Rc::clone(value)),
            _ => Key::Other,
        }
    }
}

/// A named struct type and its ordered fields.
#[derive(Clone, PartialEq, Debug)]
pub struct StructType {
    /// The struct's name in the view contract.
    pub name: String,
    /// Field names and types in declaration order.
    pub fields: Vec<(String, Type)>,
}

/// The type of a value slot, a struct field or a handler parameter.
#[derive(Clone, PartialEq, Debug)]
pub enum Type {
    /// `boolean`.
    Bool,
    /// `i32`.
    I32,
    /// `f32` or `Px`.
    F32,
    /// `f64` or plain `number`.
    F64,
    /// `string`.
    Str,
    /// An array with the given element type.
    Array(Box<Type>),
    /// A struct.
    Struct(Rc<StructType>),
}

/// Why a value does not match its declared type.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ValueError {
    /// No value slot or handler has this name.
    UnknownName,
    /// The value has a different shape than the declared type.
    TypeMismatch,
}

impl Type {
    /// Checks that `value` has exactly this type's shape.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::TypeMismatch`] at the first value that differs.
    pub fn check(&self, value: &Value) -> Result<(), ValueError> {
        match (self, value) {
            (Type::Bool, Value::Bool(_))
            | (Type::I32, Value::I32(_))
            | (Type::F32, Value::F32(_))
            | (Type::F64, Value::F64(_))
            | (Type::Str, Value::Str(_)) => Ok(()),
            (Type::Array(element), Value::Array(items)) => {
                items.iter().try_for_each(|item| element.check(item))
            }
            (Type::Struct(ty), Value::Struct(fields)) if fields.len() == ty.fields.len() => ty
                .fields
                .iter()
                .zip(fields.iter())
                .try_for_each(|((_, ty), value)| ty.check(value)),
            _ => Err(ValueError::TypeMismatch),
        }
    }

    /// The value a slot of this type holds before anything is written to it.
    #[must_use]
    pub fn default_value(&self) -> Value {
        match self {
            Type::Bool => Value::Bool(false),
            Type::I32 => Value::I32(0),
            Type::F32 => Value::F32(0.0),
            Type::F64 => Value::F64(0.0),
            Type::Str => Value::Str(Rc::from("")),
            Type::Array(_) => Value::Array(Rc::from(Vec::new())),
            Type::Struct(ty) => {
                Value::Struct(ty.fields.iter().map(|(_, ty)| ty.default_value()).collect())
            }
        }
    }
}
