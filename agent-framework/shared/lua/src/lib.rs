//! A small `no_std` Lua 5.4 wrapper built on the project's `lunka` fork.
//!
//! The public surface is deliberately narrow: register require-only libraries,
//! bind sync or async Rust functions, exchange typed tables and functions, expose
//! Rust userdata, and evaluate chunks. Async execution is executor-neutral and
//! only requires polling the returned future.
//!
//! ```
//! use claw_lua::{Lua, Result};
//!
//! # fn run() -> Result<()> {
//! let mut lua = Lua::new()?;
//! lua.register("add", |(a, b): (i64, i64)| Some(Ok(a + b)))?;
//! assert_eq!(lua.load("return add(20, 22)").eval::<i64>()?, 42);
//!
//! lua.register_lib("native", |lib| {
//!     lib.set("version", 1_i64)?;
//!     lib.register("double", |value: i64| Some(Ok(value * 2)))
//! })?;
//! let answer: i64 = lua
//!     .load("local native = require('native'); return native.double(21)")
//!     .eval()?;
//! assert_eq!(answer, 42);
//! # Ok(())
//! # }
//! # run().unwrap();
//! ```
#![no_std]

extern crate alloc;

mod convert;
mod error;
#[allow(unsafe_code)]
mod object;
mod run;
#[allow(unsafe_code)]
mod runtime;
#[allow(unsafe_code)]
mod userdata;

pub use convert::{FromLua, FromLuaMulti, IntoLua, IntoLuaMulti, Variadic};
pub use error::{Error, ErrorKind, Result};
pub use object::{Context, Function, FunctionCall, RegistryKey, Table};
pub use run::{LuaExecution, LuaInput, LuaOutput};
pub use runtime::{Chunk, Execution, Library, Lua};
pub use userdata::{
    MetaMethod, UserData, UserDataHandle, UserDataMethods, UserDataRef, UserDataRefMut,
};

/// The return contract for a Rust function exposed to Lua.
///
/// `None` produces no Lua values, `Some(Ok(value))` produces the value or
/// values, and `Some(Err(error))` produces the conventional `nil, error` pair.
pub type LuaReturn<T> = Option<Result<T>>;
