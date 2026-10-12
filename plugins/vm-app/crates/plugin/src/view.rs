//! An app's view as Lua sees it: the `view` userdata over the UI's
//! [`ViewChannel`], converting Lua values to the view's typed values and
//! handler arguments back.

use alloc::{format, string::String, vec::Vec};
use barracuda_ui_plugin::{MAX_ARRAY_ITEMS, ViewChannel, ViewError, ViewType, ViewValue};
use barracuda_vm_plugin::{Error, LuaValue, MetaMethod, Result, Table, UserData, UserDataMethods};

const MAX_NESTING: usize = 16;

/// The view userdata Lua holds:
///
/// ```lua
/// assert(view:set("count", 3))  -- or nil, message
/// view:is_open()
/// view:close()
/// ```
pub(crate) struct LuaView(pub(crate) ViewChannel);

impl LuaView {
    fn set(&self, name: &str, value: LuaValue) -> Result<()> {
        let unknown = || Error::runtime(format!("the view has no value named '{name}'"));
        let ty = self.0.value_type(name).ok_or_else(unknown)?;
        let value = to_view(value, ty, 0)
            .map_err(|expected| Error::runtime(format!("'{name}': expected {expected}")))?;
        self.0.set(name, value).map_err(|error| match error {
            ViewError::UnknownValue => unknown(),
            ViewError::Mismatch => Error::runtime(format!("'{name}': expected {}", expected(ty))),
            ViewError::Closed => Error::runtime("the view is closed"),
        })
    }
}

impl UserData for LuaView {
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("set", |view, (name, value): (String, LuaValue)| {
            Some(view.set(&name, value).map(|()| true))
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

/// Converts a Lua value for a view value of type `ty`; the error names the
/// expected type.
fn to_view(
    value: LuaValue,
    ty: &ViewType,
    depth: usize,
) -> core::result::Result<ViewValue, &'static str> {
    if depth > MAX_NESTING {
        return Err("a shallower table");
    }
    Ok(match (ty, value) {
        (ViewType::Bool, LuaValue::Boolean(value)) => ViewValue::Bool(value),
        (ViewType::I32, LuaValue::Integer(value)) => {
            ViewValue::I32(i32::try_from(value).map_err(|_| "an i32")?)
        }
        // `as` saturates and maps NaN to 0, so only integral in-range floats
        // survive the round trip.
        (ViewType::I32, LuaValue::Number(value)) if f64::from(value as i32) == value => {
            ViewValue::I32(value as i32)
        }
        (ViewType::F32, LuaValue::Integer(value)) => ViewValue::F32(value as f32),
        (ViewType::F32, LuaValue::Number(value)) => ViewValue::F32(value as f32),
        (ViewType::F64, LuaValue::Integer(value)) => ViewValue::F64(value as f64),
        (ViewType::F64, LuaValue::Number(value)) => ViewValue::F64(value),
        (ViewType::Str, LuaValue::String(value)) => ViewValue::Str(value),
        (ViewType::Array(element), LuaValue::Table(table)) => {
            ViewValue::List(array(&table, element, depth)?)
        }
        (ViewType::Struct(fields), LuaValue::Table(table)) => ViewValue::List(
            fields
                .iter()
                .map(|(name, ty)| {
                    let value = table
                        .get::<_, LuaValue>(name.as_str())
                        .map_err(|_| "a table field")?;
                    to_view(value, ty, depth + 1)
                })
                .collect::<core::result::Result<_, _>>()?,
        ),
        (ty, _) => return Err(expected(ty)),
    })
}

fn array(
    table: &Table,
    element: &ViewType,
    depth: usize,
) -> core::result::Result<Vec<ViewValue>, &'static str> {
    let len = table.len().map_err(|_| "an array")?;
    if len > MAX_ARRAY_ITEMS {
        return Err("at most 1024 items");
    }
    (1..=len)
        .map(|index| {
            let index = i64::try_from(index).map_err(|_| "an array")?;
            let value = table.get::<_, LuaValue>(index).map_err(|_| "an array")?;
            to_view(value, element, depth + 1)
        })
        .collect()
}

fn expected(ty: &ViewType) -> &'static str {
    match ty {
        ViewType::Bool => "a boolean",
        ViewType::I32 => "an integer",
        ViewType::F32 | ViewType::F64 => "a number",
        ViewType::Str => "a string",
        ViewType::Array(_) => "an array table",
        ViewType::Struct(_) => "a table",
    }
}

/// Converts a handler argument for Lua. Handler arguments are scalars in
/// practice; lists have no Lua form without a Lua context, so they arrive as
/// `nil`.
pub(crate) fn to_lua(value: ViewValue) -> LuaValue {
    match value {
        ViewValue::Bool(value) => LuaValue::Boolean(value),
        ViewValue::I32(value) => LuaValue::Integer(i64::from(value)),
        ViewValue::F32(value) => LuaValue::Number(f64::from(value)),
        ViewValue::F64(value) => LuaValue::Number(value),
        ViewValue::Str(value) => LuaValue::String(value),
        ViewValue::List(_) => LuaValue::Nil,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    extern crate std;

    use super::LuaView;
    use alloc::{format, string::String, vec::Vec};
    use barracuda_lua::FromLuaMulti;
    use barracuda_ui_plugin::{DEFAULT_VIEWPORT, UiAssets, UiService, UiSession};
    use barracuda_vm_plugin::Lua;
    use futures_lite::future;

    const VIEW: &[u8] = include_bytes!("../../../../ui/crates/runtime/tests/fixtures/counter.bvir");
    const STYLES: &[u8] =
        include_bytes!("../../../../ui/crates/runtime/tests/fixtures/counter.styles.bin");

    fn session(service: &UiService) -> UiSession {
        service
            .open(&UiAssets {
                view: VIEW.to_vec(),
                styles: STYLES.to_vec(),
                fonts: Vec::new(),
            })
            .expect("view opens")
    }

    /// A Lua state whose global `view` is the session's view.
    fn lua_with_view(session: &UiSession) -> Lua {
        let mut lua = Lua::new().expect("lua");
        let channel = session.channel().clone();
        lua.register_with("get_view", move |lua, (): ()| {
            Some(lua.create_userdata(LuaView(channel.clone())))
        })
        .expect("register");
        lua.load("view = get_view()").exec().expect("view");
        lua
    }

    fn eval<T: FromLuaMulti>(lua: &mut Lua, source: &str) -> T {
        future::block_on(lua.load(source).eval_async::<T>()).expect(source)
    }

    #[test]
    fn lua_tables_and_numbers_become_view_values() {
        let service = UiService::new(DEFAULT_VIEWPORT);
        let session = session(&service);
        let mut lua = lua_with_view(&session);
        eval::<()>(
            &mut lua,
            "assert(view:set('count', 2)) \
             assert(view:set('history', { { id = 2, label = 'two' }, { id = 1, label = 'one' } }))",
        );
        assert_eq!(session.texts(), ["2", "+1", "Reset", "two", "one"]);

        // Integral floats are accepted for integer values.
        eval::<()>(&mut lua, "assert(view:set('count', 0.0))");
        assert_eq!(session.texts(), ["0", "+1", "Reset"]);
    }

    #[test]
    fn lua_writes_are_checked_against_the_contract() {
        let service = UiService::new(DEFAULT_VIEWPORT);
        let session = session(&service);
        let mut lua = lua_with_view(&session);
        for (source, message) in [
            ("view:set('count', 'two')", "'count': expected an integer"),
            ("view:set('count', 1.5)", "'count': expected an integer"),
            (
                "view:set('missing', 1)",
                "the view has no value named 'missing'",
            ),
            (
                "view:set('history', { { id = 'x', label = 'y' } })",
                "'history': expected an integer",
            ),
            (
                "view:set('history', 3)",
                "'history': expected an array table",
            ),
        ] {
            let (ok, error): (Option<bool>, String) = eval(&mut lua, &format!("return {source}"));
            assert_eq!(ok, None, "{source}");
            assert_eq!(error, message, "{source}");
        }
        assert_eq!(session.texts(), ["0", "+1", "Reset"]);
    }

    #[test]
    fn closing_from_lua_ends_the_view() {
        let service = UiService::new(DEFAULT_VIEWPORT);
        let session = session(&service);
        let mut lua = lua_with_view(&session);
        eval::<()>(&mut lua, "view:close()");
        assert!(session.is_closed());
        let (is_open, set): (bool, bool) = eval(
            &mut lua,
            "return view:is_open(), view:set('count', 1) ~= nil",
        );
        assert_eq!((is_open, set), (false, false));
    }
}
