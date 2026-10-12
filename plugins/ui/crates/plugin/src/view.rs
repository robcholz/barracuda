//! The Lua side of a view: a `Send + Sync` channel to the UI task and the
//! userdata Lua holds.

use crate::datum::{Datum, Shape};
use crate::mailbox::{Mailbox, MailboxError, NextEvent, Schema};
use alloc::{format, string::String, sync::Arc, vec::Vec};
use barracuda_vm_plugin::{Error, LuaValue, MetaMethod, Result, Table, UserData, UserDataMethods};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

/// The longest array Lua may write into one slot.
pub const MAX_ARRAY_ITEMS: usize = 1024;
const MAX_NESTING: usize = 16;

/// One handler call from a view, converted for Lua.
pub struct ViewCall {
    /// The handler name from the view contract.
    pub handler: String,
    /// Its arguments; list arguments arrive as `nil`.
    pub args: Vec<LuaValue>,
}

/// A view as Lua packages see it: write values, wait for handler calls.
///
/// Cloning shares the same view. Closing it ends the view for Lua; the UI
/// keeps the session until the owner drops it.
#[derive(Clone)]
pub struct ViewChannel {
    pub(crate) id: u32,
    pub(crate) schema: Arc<Schema>,
    pub(crate) mailbox: Arc<Mailbox>,
}

impl ViewChannel {
    /// Writes a value declared by the view contract.
    ///
    /// # Errors
    ///
    /// Fails when the view has no such value, `value` does not match its type,
    /// or the view is closed.
    pub fn set(&self, name: &str, value: LuaValue) -> Result<()> {
        let (slot, shape) = self
            .schema
            .slots
            .iter()
            .enumerate()
            .find_map(|(index, (slot, shape))| (slot == name).then_some((index, shape)))
            .ok_or_else(|| Error::runtime(format!("the view has no value named '{name}'")))?;
        let datum = to_datum(value, shape, 0)
            .map_err(|expected| Error::runtime(format!("'{name}': expected {expected}")))?;
        self.mailbox
            .write(self.id, slot, datum)
            .map_err(|error| match error {
                MailboxError::Closed => Error::runtime("the view is closed"),
                MailboxError::UnknownSlot => {
                    Error::runtime(format!("the view has no value named '{name}'"))
                }
            })
    }

    /// Waits for the next handler call; `None` once the view is closed.
    #[must_use]
    pub fn next(&self) -> ViewNext {
        ViewNext {
            inner: self.mailbox.next(self.id),
        }
    }

    /// Whether the view still takes writes and delivers calls.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.mailbox.is_closed(self.id)
    }

    /// Ends the view for Lua.
    pub fn close(&self) {
        self.mailbox.close(self.id);
    }
}

/// The future behind [`ViewChannel::next`].
pub struct ViewNext {
    inner: NextEvent,
}

impl Future for ViewNext {
    type Output = Option<ViewCall>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.inner).poll(cx).map(|event| {
            event.map(|event| ViewCall {
                handler: event.handler,
                args: event.args.into_iter().map(to_lua).collect(),
            })
        })
    }
}

/// The view userdata Lua holds:
///
/// ```lua
/// assert(view:set("count", 3))  -- or nil, message
/// view:is_open()
/// view:close()
/// ```
pub struct LuaView(pub ViewChannel);

impl UserData for LuaView {
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("set", |view, (name, value): (String, LuaValue)| {
            Some(view.0.set(&name, value).map(|()| true))
        });
        methods.add_method("is_open", |view, (): ()| Some(Ok(view.0.is_open())));
        methods.add_method("close", |view, (): ()| {
            view.0.close();
            None::<Result<()>>
        });
        methods.add_meta_method(MetaMethod::Close, |view, _error: Option<String>| {
            view.0.close();
            None::<Result<()>>
        });
    }
}

/// Converts a Lua value for a slot of `shape`; the error names the expected type.
fn to_datum(
    value: LuaValue,
    shape: &Shape,
    depth: usize,
) -> core::result::Result<Datum, &'static str> {
    if depth > MAX_NESTING {
        return Err("a shallower table");
    }
    Ok(match (shape, value) {
        (Shape::Bool, LuaValue::Boolean(value)) => Datum::Bool(value),
        (Shape::I32, LuaValue::Integer(value)) => {
            Datum::I32(i32::try_from(value).map_err(|_| "an i32")?)
        }
        // `as` saturates and maps NaN to 0, so only integral in-range floats
        // survive the round trip.
        (Shape::I32, LuaValue::Number(value)) if f64::from(value as i32) == value => {
            Datum::I32(value as i32)
        }
        (Shape::F32, LuaValue::Integer(value)) => Datum::F32(value as f32),
        (Shape::F32, LuaValue::Number(value)) => Datum::F32(value as f32),
        (Shape::F64, LuaValue::Integer(value)) => Datum::F64(value as f64),
        (Shape::F64, LuaValue::Number(value)) => Datum::F64(value),
        (Shape::Str, LuaValue::String(value)) => Datum::Str(value),
        (Shape::Array(element), LuaValue::Table(table)) => {
            Datum::List(array(&table, element, depth)?)
        }
        (Shape::Struct(fields), LuaValue::Table(table)) => Datum::List(
            fields
                .iter()
                .map(|(name, shape)| {
                    let value = table
                        .get::<_, LuaValue>(name.as_str())
                        .map_err(|_| "a table field")?;
                    to_datum(value, shape, depth + 1)
                })
                .collect::<core::result::Result<_, _>>()?,
        ),
        (shape, _) => return Err(expected(shape)),
    })
}

fn array(
    table: &Table,
    element: &Shape,
    depth: usize,
) -> core::result::Result<Vec<Datum>, &'static str> {
    let len = table.len().map_err(|_| "an array")?;
    if len > MAX_ARRAY_ITEMS {
        return Err("at most 1024 items");
    }
    (1..=len)
        .map(|index| {
            let index = i64::try_from(index).map_err(|_| "an array")?;
            let value = table.get::<_, LuaValue>(index).map_err(|_| "an array")?;
            to_datum(value, element, depth + 1)
        })
        .collect()
}

fn expected(shape: &Shape) -> &'static str {
    match shape {
        Shape::Bool => "a boolean",
        Shape::I32 => "an integer",
        Shape::F32 | Shape::F64 => "a number",
        Shape::Str => "a string",
        Shape::Array(_) => "an array table",
        Shape::Struct(_) => "a table",
    }
}

/// Handler arguments are scalars in practice; lists have no Lua form without a
/// Lua context, so they arrive as `nil`.
fn to_lua(datum: Datum) -> LuaValue {
    match datum {
        Datum::Bool(value) => LuaValue::Boolean(value),
        Datum::I32(value) => LuaValue::Integer(i64::from(value)),
        Datum::F32(value) => LuaValue::Number(f64::from(value)),
        Datum::F64(value) => LuaValue::Number(value),
        Datum::Str(value) => LuaValue::String(value),
        Datum::List(_) => LuaValue::Nil,
    }
}
