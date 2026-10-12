//! The `app` Lua package: an app's lifecycle, view, timer, and storage.
//!
//! The App Manager starts every app program with
//! `require("app").__bind("<ticket>");` on its first line. Binding fills the
//! package table, so `require("app")` returns the running app everywhere in
//! the program. `docs/app-format.md` documents the API.

use crate::port::{AppEvent, MIN_TICK_MS, Port, Ports};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use barracuda_ui_plugin::LuaView;
use barracuda_vm_plugin::{
    Context, Error, Lua, LuaPackage, LuaValue, Package, Result, UserData, UserDataHandle,
    UserDataMethods, Variadic,
};
use core::sync::atomic::{AtomicBool, Ordering};
use embassy_time::Duration;
use serde_json::{Map, Number, Value};

const MAX_DEPTH: usize = 16;

pub(crate) struct AppPackage {
    ports: Arc<Ports>,
    active: Arc<AtomicBool>,
}

impl AppPackage {
    pub(crate) fn new(ports: Arc<Ports>) -> Self {
        Self {
            ports,
            active: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl Package for AppPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let ports = Arc::clone(&self.ports);
        let active = Arc::clone(&self.active);
        lua.register_lib("app", move |app| {
            app.register_with("__attach", move |lua, ticket: String| {
                if !active.load(Ordering::Acquire) {
                    return Some(Err(Error::runtime("the app package has been revoked")));
                }
                Some(match ports.attach(&ticket) {
                    Some(port) => lua.create_userdata(AppHandle { port }),
                    None => Err(Error::runtime("this program is not a running app")),
                })
            })?;
            app.register_with("__view", |lua, handle: UserDataHandle<AppHandle>| {
                let view = match handle.with(|handle| handle.port.view.clone()) {
                    Ok(view) => view,
                    Err(error) => return Some(Err(error)),
                };
                Some(
                    view.map(|channel| lua.create_userdata(LuaView(channel)))
                        .transpose(),
                )
            })?;
            app.register("__encode", |value: LuaValue| {
                Some(
                    encode(value, 0)
                        .map(|value| value.to_string())
                        .map_err(Error::runtime),
                )
            })?;
            app.register_with("__decode", |lua, json: String| {
                Some(
                    serde_json::from_str::<Value>(&json)
                        .map_err(|_| Error::runtime("stored data is damaged"))
                        .and_then(|value| decode(lua, value)),
                )
            })
        })?;
        lua.load(ADAPTER).exec()
    }
}

impl LuaPackage for AppPackage {
    fn name(&self) -> &'static str {
        "app"
    }

    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        self.ports.end_all();
    }
}

/// The native half of a bound app; the adapter wraps it.
pub(crate) struct AppHandle {
    port: Arc<Port>,
}

fn port(handle: &UserDataHandle<AppHandle>) -> Result<Arc<Port>> {
    handle.with(|handle| Arc::clone(&handle.port))
}

/// `Ok(value)` or Lua's `nil, message`.
fn lua_result<T>(result: core::result::Result<T, String>) -> Option<Result<T>> {
    Some(result.map_err(Error::runtime))
}

impl UserData for AppHandle {
    fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
        methods.add_method("id", |app, (): ()| Some(Ok(app.port.id.clone())));
        methods.add_method("name", |app, (): ()| Some(Ok(app.port.name.clone())));
        methods.add_method("version", |app, (): ()| Some(Ok(app.port.version.clone())));
        methods.add_method("set_every", |app, every: Option<i64>| {
            let every = match every {
                None => None,
                Some(ms) if u64::try_from(ms).is_ok_and(|ms| ms >= MIN_TICK_MS) => {
                    Some(Duration::from_millis(ms.unsigned_abs()))
                }
                Some(_) => {
                    return Some(Err(Error::runtime(format!(
                        "every is at least {MIN_TICK_MS} milliseconds"
                    ))));
                }
            };
            app.port.set_every(every);
            Some(Ok(true))
        });
        methods.add_method("exit", |app, (): ()| {
            app.port.end();
            None::<Result<()>>
        });
        methods.add_async_method("next", |handle, (): ()| async move {
            let port = match port(&handle) {
                Ok(port) => port,
                Err(error) => return Some(Err(error)),
            };
            let values = match port.next().await {
                None => Vec::new(),
                Some(AppEvent::Lifecycle(event)) => {
                    alloc::vec![LuaValue::String(String::from(event.name()))]
                }
                Some(AppEvent::Tick) => alloc::vec![LuaValue::String(String::from("app:tick"))],
                Some(AppEvent::View(call)) => {
                    let mut values = Vec::with_capacity(call.args.len() + 1);
                    values.push(LuaValue::String(call.handler));
                    values.extend(call.args);
                    values
                }
            };
            Some(Ok(Variadic::from(values)))
        });
        methods.add_async_method("kv_get", |handle, key: String| async move {
            let port = match port(&handle) {
                Ok(port) => port,
                Err(error) => return Some(Err(error)),
            };
            lua_result(port.storage.kv_get(&key).await)
        });
        methods.add_async_method(
            "kv_set",
            |handle, (key, json): (String, Option<String>)| async move {
                let port = match port(&handle) {
                    Ok(port) => port,
                    Err(error) => return Some(Err(error)),
                };
                lua_result(
                    port.storage
                        .kv_set(&key, json.as_deref())
                        .await
                        .map(|()| true),
                )
            },
        );
        methods.add_async_method("kv_keys", |handle, (): ()| async move {
            let port = match port(&handle) {
                Ok(port) => port,
                Err(error) => return Some(Err(error)),
            };
            lua_result(port.storage.kv_keys().await)
        });
        methods.add_async_method("fs_read", |handle, path: String| async move {
            let port = match port(&handle) {
                Ok(port) => port,
                Err(error) => return Some(Err(error)),
            };
            lua_result(port.storage.read(&path).await)
        });
        methods.add_async_method(
            "fs_write",
            |handle, (path, data, append): (String, Vec<u8>, bool)| async move {
                let port = match port(&handle) {
                    Ok(port) => port,
                    Err(error) => return Some(Err(error)),
                };
                lua_result(
                    port.storage
                        .write(&path, &data, append)
                        .await
                        .map(|()| true),
                )
            },
        );
        methods.add_async_method("fs_list", |handle, path: Option<String>| async move {
            let port = match port(&handle) {
                Ok(port) => port,
                Err(error) => return Some(Err(error)),
            };
            lua_result(port.storage.list(path.as_deref().unwrap_or("")).await)
        });
        methods.add_async_method("fs_remove", |handle, path: String| async move {
            let port = match port(&handle) {
                Ok(port) => port,
                Err(error) => return Some(Err(error)),
            };
            lua_result(port.storage.remove(&path).await.map(|()| true))
        });
        methods.add_async_method("fs_exists", |handle, path: String| async move {
            let port = match port(&handle) {
                Ok(port) => port,
                Err(error) => return Some(Err(error)),
            };
            lua_result(port.storage.exists(&path).await)
        });
        methods.add_async_method("fs_mkdir", |handle, path: String| async move {
            let port = match port(&handle) {
                Ok(port) => port,
                Err(error) => return Some(Err(error)),
            };
            lua_result(port.storage.mkdir(&path).await.map(|()| true))
        });
    }
}

/// Lua values the key-value store keeps: booleans, numbers, strings, and
/// tables of them that are arrays or have string keys.
fn encode(value: LuaValue, depth: usize) -> core::result::Result<Value, String> {
    if depth > MAX_DEPTH {
        return Err(format!("values nest at most {MAX_DEPTH} tables deep"));
    }
    Ok(match value {
        LuaValue::Nil => Value::Null,
        LuaValue::Boolean(value) => Value::Bool(value),
        LuaValue::Integer(value) => Value::Number(Number::from(value)),
        LuaValue::Number(value) => Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| String::from("numbers must be finite"))?,
        LuaValue::String(value) => Value::String(value),
        LuaValue::Table(table) => {
            let pairs = table.pairs().map_err(|_| {
                String::from("tables hold only booleans, numbers, strings, and tables")
            })?;
            let is_array = pairs.iter().all(|(key, _)| {
                matches!(key, LuaValue::Integer(index)
                    if usize::try_from(*index).is_ok_and(|index| (1..=pairs.len()).contains(&index)))
            });
            if is_array {
                let mut items: Vec<(i64, LuaValue)> = pairs
                    .into_iter()
                    .filter_map(|(key, value)| match key {
                        LuaValue::Integer(index) => Some((index, value)),
                        _ => None,
                    })
                    .collect();
                items.sort_unstable_by_key(|(index, _)| *index);
                Value::Array(
                    items
                        .into_iter()
                        .map(|(_, value)| encode(value, depth + 1))
                        .collect::<core::result::Result<_, _>>()?,
                )
            } else {
                let mut object = Map::new();
                for (key, value) in pairs {
                    let LuaValue::String(key) = key else {
                        return Err(String::from(
                            "a table is either an array or has only string keys",
                        ));
                    };
                    object.insert(key, encode(value, depth + 1)?);
                }
                Value::Object(object)
            }
        }
    })
}

fn decode(lua: &mut Context<'_>, value: Value) -> Result<LuaValue> {
    Ok(match value {
        Value::Null => LuaValue::Nil,
        Value::Bool(value) => LuaValue::Boolean(value),
        Value::Number(number) => match number.as_i64() {
            Some(value) => LuaValue::Integer(value),
            None => LuaValue::Number(number.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(value) => LuaValue::String(value),
        Value::Array(items) => {
            let table = lua.create_table()?;
            for (index, item) in (1_i64..).zip(items) {
                table.set(index, decode(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
        Value::Object(object) => {
            let table = lua.create_table()?;
            for (key, item) in object {
                table.set(key, decode(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
    })
}

/// Builds the public `app` table on the native functions.
const ADAPTER: &str = r##"
local app = require("app")
local attach, view_of, encode, decode = app.__attach, app.__view, app.__encode, app.__decode
app.__attach, app.__view, app.__encode, app.__decode = nil, nil, nil, nil

local function not_bound()
  error("this program is not a running app", 2)
end
setmetatable(app, { __index = function() return not_bound end })

function app.__bind(ticket)
  app.__bind = nil
  local handle = assert(attach(ticket))
  setmetatable(app, nil)
  app.id, app.name, app.version = handle:id(), handle:name(), handle:version()

  local view = view_of(handle)
  function app.view() return view end

  function app.events(options)
    if options ~= nil and options.every ~= nil then
      local ok, message = handle:set_every(options.every)
      if not ok then error(message, 2) end
    end
    return function() return handle:next() end
  end

  function app.exit() handle:exit() end

  local function decoded(json, message)
    if json ~= nil then return decode(json) end
    if message ~= nil then return nil, message end
    return nil
  end

  local storable = { boolean = true, number = true, string = true, table = true }
  app.kv = {
    get = function(key) return decoded(handle:kv_get(key)) end,
    set = function(key, value)
      if value == nil then return handle:kv_set(key, nil) end
      if not storable[type(value)] then
        return nil, "values are booleans, numbers, strings, and tables"
      end
      local json, message = encode(value)
      if json == nil then return nil, message end
      return handle:kv_set(key, json)
    end,
    delete = function(key) return handle:kv_set(key, nil) end,
    keys = function() return decoded(handle:kv_keys()) end,
  }

  app.fs = {
    read = function(path) return handle:fs_read(path) end,
    write = function(path, data) return handle:fs_write(path, data, false) end,
    append = function(path, data) return handle:fs_write(path, data, true) end,
    list = function(path) return decoded(handle:fs_list(path)) end,
    remove = function(path) return handle:fs_remove(path) end,
    exists = function(path) return handle:fs_exists(path) end,
    mkdir = function(path) return handle:fs_mkdir(path) end,
  }
end
"##;
