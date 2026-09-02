//! Example library that demonstrates
//! the exposing of some Rust functionality to Lua
//! in an importable library.

use lunka::prelude::*;
use std::{fs::metadata, time::SystemTime};

lua_export_fn!(luaopen_os2);
fn luaopen_os2(lua: &mut LuaThread) -> LuaRets {
    lua.managed().new_lib(&LIBRARY);
    1.into()
}

const LIBRARY: LuaLibrary = lua_library! {
    metadata: l_metadata,
};

extern "C-unwind" fn l_metadata(mut lua: LuaCtx<'_>) -> LuaRets {
    let path = lua.check_string(1);

    let meta = match metadata(String::from_utf8_lossy(path).as_ref()) {
        Ok(meta) => meta,
        Err(error) => {
            lua.push_fail();
            lua.managed().push_display(&error);
            return 2.into();
        }
    };

    let mut mg = lua.managed();
    mg.create_table(0, 1);

    let file_type = meta.file_type();
    mg.push_string(
        if file_type.is_file() {
            "file"
        } else if file_type.is_dir() {
            "directory"
        } else if file_type.is_symlink() {
            "symlink"
        } else {
            "other"
        }
        .as_bytes(),
    );
    unsafe { mg.set_field(-2, c"type") };

    mg.push_integer(meta.len() as _);
    unsafe { mg.set_field(-2, c"len") };

    if let Ok(time) = meta.modified()
        && let Ok(time) = time.duration_since(SystemTime::UNIX_EPOCH)
    {
        mg.push_number(time.as_secs_f64());
        unsafe { mg.set_field(-2, c"modified") };
    }

    1.into()
}
