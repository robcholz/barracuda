//! Thread-safe mirrors of view values and types.
//!
//! [`Value`] and [`Type`] use `Rc` because they live in the single-threaded UI
//! tree. Lua packages must be `Send + Sync`, so values cross from Lua to the UI
//! as [`Datum`]s checked against [`Shape`]s.

use alloc::{string::String, sync::Arc, vec::Vec};
use barracuda_ui_runtime::{Type, Value};

/// A value on its way between Lua and a view.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Datum {
    Bool(bool),
    I32(i32),
    F32(f32),
    F64(f64),
    Str(String),
    /// An array's items or a struct's fields in declaration order.
    List(Vec<Datum>),
}

/// The type of a slot, field or handler parameter, shareable across threads.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Shape {
    Bool,
    I32,
    F32,
    F64,
    Str,
    Array(Arc<Shape>),
    Struct(Arc<Vec<(String, Shape)>>),
}

impl From<&Type> for Shape {
    fn from(ty: &Type) -> Self {
        match ty {
            Type::Bool => Shape::Bool,
            Type::I32 => Shape::I32,
            Type::F32 => Shape::F32,
            Type::F64 => Shape::F64,
            Type::Str => Shape::Str,
            Type::Array(element) => Shape::Array(Arc::new(Shape::from(&**element))),
            Type::Struct(st) => Shape::Struct(Arc::new(
                st.fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), Shape::from(ty)))
                    .collect(),
            )),
        }
    }
}

impl Datum {
    /// Rebuilds a view value; `None` when the datum does not fit `ty`.
    pub(crate) fn into_value(self, ty: &Type) -> Option<Value> {
        Some(match (self, ty) {
            (Datum::Bool(value), Type::Bool) => Value::Bool(value),
            (Datum::I32(value), Type::I32) => Value::I32(value),
            (Datum::F32(value), Type::F32) => Value::F32(value),
            (Datum::F64(value), Type::F64) => Value::F64(value),
            (Datum::Str(value), Type::Str) => Value::Str(value.into()),
            (Datum::List(items), Type::Array(element)) => Value::Array(
                items
                    .into_iter()
                    .map(|item| item.into_value(element))
                    .collect::<Option<Vec<_>>>()?
                    .into(),
            ),
            (Datum::List(fields), Type::Struct(st)) if fields.len() == st.fields.len() => {
                Value::Struct(
                    fields
                        .into_iter()
                        .zip(&st.fields)
                        .map(|(field, (_, ty))| field.into_value(ty))
                        .collect::<Option<Vec<_>>>()?
                        .into(),
                )
            }
            _ => return None,
        })
    }

    /// Copies a view value; structs and arrays become lists.
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        Some(match value {
            Value::Bool(value) => Datum::Bool(*value),
            Value::I32(value) => Datum::I32(*value),
            Value::F32(value) => Datum::F32(*value),
            Value::F64(value) => Datum::F64(*value),
            Value::Str(value) => Datum::Str(String::from(&**value)),
            Value::Struct(items) | Value::Array(items) => Datum::List(
                items
                    .iter()
                    .map(Datum::from_value)
                    .collect::<Option<Vec<_>>>()?,
            ),
            Value::Void => return None,
        })
    }
}
