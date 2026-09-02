//! (Very) simple Lua interpreter.

use lunka::prelude::*;
use std::{
    env::args,
    ffi::{CStr, c_uint},
    io::{Write, stderr},
    process::ExitCode,
};

fn main() -> ExitCode {
    let mut lua = Lua::new();

    lua.push_function(l_main);
    let status = unsafe { lua.managed().pcall(0, 1, 0) };
    let is_ok = lua.to_boolean(-1);
    report(&mut lua, status);

    if status.is_ok() && is_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

extern "C-unwind" fn l_main(mut lua: LuaCtx<'_>) -> LuaRets {
    lua.check_version();
    lua.managed().open_libs();

    lua.push_function(l_err_handler);
    let base = lua.top();

    let mut arguments = args().skip(1);
    let load_status = if let Some(mut file_name) = arguments.next() {
        lua.managed().load_file(unsafe {
            file_name.push('\0');
            CStr::from_bytes_until_nul(file_name.as_bytes()).unwrap_unchecked()
        })
    } else {
        lua.managed().load_stdin()
    };

    if !report(&mut lua, load_status) {
        return 0.into();
    }

    let mut arg_count: c_uint = 0;
    for arg in arguments {
        lua.managed().push_string(arg.as_bytes());
        arg_count += 1;
    }

    let mut mg = lua.managed();
    mg.restart_gc();
    let run_status = unsafe { mg.pcall(arg_count, 0, base) };
    mg.stop_gc();
    if !report(&mut lua, run_status) {
        return 0.into();
    }

    lua.push_boolean(true);
    1.into()
}

extern "C-unwind" fn l_err_handler(mut lua: LuaCtx<'_>) -> LuaRets {
    unsafe {
        if let Some(msg) = lua.managed_no_gc().to_c_str(1) {
            lua.managed_no_gc().traceback_self(Some(msg), 1);
            return 1.into();
        }
    }

    let ok = unsafe { lua.managed().call_metamethod(1, c"__tostring") };
    if ok && lua.type_of(-1) == LuaType::String {
        return 1.into();
    }

    unsafe { lua_push_fmt_string!(lua, c"(error object is a %s value)", lua.type_name_of(1)) };

    1.into()
}

fn report(lua: &mut LuaThread, status: LuaStatus) -> bool {
    if !status.is_ok() {
        if let Some(message) = lua.managed().to_c_str(-1) {
            c_eprintln(message);
        }
        unsafe { lua.managed().pop(1) };
        false
    } else {
        true
    }
}

fn c_eprintln(data: &CStr) {
    let mut out = stderr();
    let _ = out.write_all(data.to_bytes());
    let _ = out.write_all(b"\n");
}
