//! Thread-safe mirrors of view values and types.
//!
//! [`Value`] and [`Type`] use `Rc` because they live in the single-threaded UI
//! tree. A view's owner may run elsewhere, so values cross to the UI as
//! [`ViewValue`]s checked against [`ViewType`]s.

use alloc::{string::String, vec::Vec};
use barracuda_ui_runtime::{Type, Value};
use portable_atomic_util::Arc;

/// A value on its way between a view and its owner.
#[derive(Clone, PartialEq, Debug)]
pub enum ViewValue {
    /// A `boolean`.
    Bool(bool),
    /// An `i32`.
    I32(i32),
    /// An `f32`.
    F32(f32),
    /// An `f64`.
    F64(f64),
    /// A `string`.
    Str(String),
    /// An array's items or a struct's fields in declaration order.
    List(Vec<ViewValue>),
}

/// The type of a value, field or handler parameter, shareable across threads.
#[derive(Clone, PartialEq, Debug)]
pub enum ViewType {
    /// `boolean`.
    Bool,
    /// `i32`.
    I32,
    /// `f32`.
    F32,
    /// `f64`.
    F64,
    /// `string`.
    Str,
    /// An array of at most [`MAX_ARRAY_ITEMS`](crate::MAX_ARRAY_ITEMS) items.
    Array(Arc<ViewType>),
    /// A struct's fields, named, in declaration order.
    Struct(Arc<Vec<(String, ViewType)>>),
}

impl From<&Type> for ViewType {
    fn from(ty: &Type) -> Self {
        match ty {
            Type::Bool => ViewType::Bool,
            Type::I32 => ViewType::I32,
            Type::F32 => ViewType::F32,
            Type::F64 => ViewType::F64,
            Type::Str => ViewType::Str,
            Type::Array(element) => ViewType::Array(Arc::new(ViewType::from(&**element))),
            Type::Struct(st) => ViewType::Struct(Arc::new(
                st.fields
                    .iter()
                    .map(|(name, ty)| (name.clone(), ViewType::from(ty)))
                    .collect(),
            )),
        }
    }
}

impl ViewValue {
    /// Whether the value has type `ty`, arrays within
    /// [`MAX_ARRAY_ITEMS`](crate::MAX_ARRAY_ITEMS).
    #[must_use]
    pub fn fits(&self, ty: &ViewType) -> bool {
        match (self, ty) {
            (ViewValue::Bool(_), ViewType::Bool)
            | (ViewValue::I32(_), ViewType::I32)
            | (ViewValue::F32(_), ViewType::F32)
            | (ViewValue::F64(_), ViewType::F64)
            | (ViewValue::Str(_), ViewType::Str) => true,
            (ViewValue::List(items), ViewType::Array(element)) => {
                items.len() <= crate::MAX_ARRAY_ITEMS && items.iter().all(|item| item.fits(element))
            }
            (ViewValue::List(items), ViewType::Struct(fields)) => {
                items.len() == fields.len()
                    && items
                        .iter()
                        .zip(fields.iter())
                        .all(|(item, (_, ty))| item.fits(ty))
            }
            _ => false,
        }
    }

    /// Rebuilds a view value; `None` when the datum does not fit `ty`.
    pub(crate) fn into_value(self, ty: &Type) -> Option<Value> {
        Some(match (self, ty) {
            (ViewValue::Bool(value), Type::Bool) => Value::Bool(value),
            (ViewValue::I32(value), Type::I32) => Value::I32(value),
            (ViewValue::F32(value), Type::F32) => Value::F32(value),
            (ViewValue::F64(value), Type::F64) => Value::F64(value),
            (ViewValue::Str(value), Type::Str) => Value::Str(value.into()),
            (ViewValue::List(items), Type::Array(element)) => Value::Array(
                items
                    .into_iter()
                    .map(|item| item.into_value(element))
                    .collect::<Option<Vec<_>>>()?
                    .into(),
            ),
            (ViewValue::List(fields), Type::Struct(st)) if fields.len() == st.fields.len() => {
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
            Value::Bool(value) => ViewValue::Bool(*value),
            Value::I32(value) => ViewValue::I32(*value),
            Value::F32(value) => ViewValue::F32(*value),
            Value::F64(value) => ViewValue::F64(*value),
            Value::Str(value) => ViewValue::Str(String::from(&**value)),
            Value::Struct(items) | Value::Array(items) => ViewValue::List(
                items
                    .iter()
                    .map(ViewValue::from_value)
                    .collect::<Option<Vec<_>>>()?,
            ),
            Value::Void => return None,
        })
    }
}
