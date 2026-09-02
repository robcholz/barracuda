use lunka::prelude::*;

fn main() {
    let mut lua = Lua::new();

    let mut mg = lua.managed();
    mg.push_function(l_main);
    let did_run_ok = if unsafe { mg.pcall(0, 1, 0).is_ok() } {
        mg.to_boolean(-1)
    } else {
        false
    };
    if !did_run_ok {
        panic!("couldn't run \"Hello, world!\" example for some reason");
    }
}

extern "C-unwind" fn l_main(mut lua: LuaCtx<'_>) -> LuaRets {
    lua.managed().open_libs();

    let is_ok = lua
        .managed()
        .load_string(r#"print("Hello, world!")"#, c"=<embedded>")
        .is_ok();
    if !is_ok {
        let mut lua = lua.managed();
        let error = {
            lua.to_string(-1)
                .and_then(move |bytes| core::str::from_utf8(bytes).ok())
                .unwrap_or("<message is not UTF-8>")
        };
        eprintln!("couldn't load example Lua code:\n\t{error}");
        lua.push_boolean(false);
        return 1.into();
    }

    let is_ok = unsafe { lua.managed().pcall(0, 0, 0).is_ok() };
    if !is_ok {
        let mut lua = lua.managed();
        let error = {
            lua.to_string(-1)
                .and_then(move |bytes| core::str::from_utf8(bytes).ok())
                .unwrap_or("<message is not UTF-8>")
        };
        eprintln!("couldn't run example Lua code:\n\t{error}");
        lua.push_boolean(false);
        return 1.into();
    }

    lua.push_boolean(true);
    1.into()
}
