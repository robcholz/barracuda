//! The `ui` Lua package.
//!
//! ```lua
//! local view = assert(require("ui").attach(APP.ui)) -- claim the view prepared for this run
//! assert(view:set("count", 3))                       -- write a value slot
//! local name, a, b = view:next()                     -- wait for a handler call; nil once closed
//! view:close()
//! ```
//!
//! Failures follow the Lua convention of returning `nil` and a message:
//!
//! ```lua
//! local ok, message = view:set("count", "three") -- nil, "'count': expected an integer"
//! ```

use crate::datum::{Datum, Shape};
use crate::mailbox::{Mailbox, MailboxError, Schema};
use alloc::{format, string::String, sync::Arc, vec::Vec};
use barracuda_vm_plugin::{
    Error, Lua, LuaPackage, LuaValue, MetaMethod, Package, Result, Table, UserData,
    UserDataMethods, Variadic,
};
use core::sync::atomic::{AtomicBool, Ordering};

/// The longest array Lua may write into one slot.
pub const MAX_ARRAY_ITEMS: usize = 1024;
const MAX_NESTING: usize = 16;

pub(crate) struct UiPackage {
    mailbox: Arc<Mailbox>,
    active: Arc<AtomicBool>,
}

impl UiPackage {
    pub(crate) fn new(mailbox: Arc<Mailbox>) -> Self {
        Self {
            mailbox,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Package for UiPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let mailbox = Arc::clone(&self.mailbox);
        let active = Arc::clone(&self.active);
        lua.register_lib("ui", move |package| {
            package.register_with("attach", move |lua, ticket: String| {
                if !active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("ui package has been revoked")));
                }
                Some(match mailbox.attach(&ticket) {
                    Some((id, schema)) => lua.create_userdata(ViewHandle {
                        id,
                        schema,
                        mailbox: Arc::clone(&mailbox),
                        active: Arc::clone(&active),
                    }),
                    None => Err(Error::runtime("no view is waiting for this ticket")),
                })
            })
        })
    }
}

impl LuaPackage for UiPackage {
    fn name(&self) -> &'static str {
        "ui"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        self.mailbox.close_all();
    }
}

struct ViewHandle {
    id: u32,
    schema: Arc<Schema>,
    mailbox: Arc<Mailbox>,
    active: Arc<AtomicBool>,
}

impl ViewHandle {
    fn set(&self, name: &str, value: LuaValue) -> Result<bool> {
        if !self.active.load(Ordering::Acquire) {
            return Err(Error::runtime("ui package has been revoked"));
        }
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
            .map(|()| true)
            .map_err(|error| match error {
                MailboxError::Closed => Error::runtime("the view is closed"),
                MailboxError::UnknownSlot => {
                    Error::runtime(format!("the view has no value named '{name}'"))
                }
            })
    }
}

impl UserData for ViewHandle {
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("set", |view, (name, value): (String, LuaValue)| {
            Some(view.set(&name, value))
        });
        methods.add_async_method("next", |view, (): ()| async move {
            let next = {
                let view = match view.borrow() {
                    Ok(view) => view,
                    Err(error) => return Some(Err(error)),
                };
                view.mailbox.next(view.id)
            };
            let Some(event) = next.await else {
                return Some(Ok(Variadic::from(Vec::new())));
            };
            let mut values = Vec::with_capacity(event.args.len() + 1);
            values.push(LuaValue::String(event.handler));
            values.extend(event.args.into_iter().map(to_lua));
            Some(Ok(Variadic::from(values)))
        });
        methods.add_method("is_open", |view, (): ()| {
            Some(Ok(!view.mailbox.is_closed(view.id)))
        });
        methods.add_method("close", |view, (): ()| {
            view.mailbox.close(view.id);
            None::<Result<()>>
        });
        methods.add_meta_method(MetaMethod::Close, |view, _error: Option<String>| {
            view.mailbox.close(view.id);
            None::<Result<()>>
        });
    }
}

impl Drop for ViewHandle {
    fn drop(&mut self) {
        self.mailbox.close(self.id);
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

/// Event arguments are scalars in practice; lists have no Lua form without a
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
