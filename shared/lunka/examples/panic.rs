use lunka::prelude::*;

fn main() {
    std::panic::catch_unwind(move || {
        let mut lua = Lua::new();
        lua.at_panic(Some(lunka::rust_panic_handler));
        lua.push_function(lua_func!(lua => {
            struct Guard;
            impl Drop for Guard {
                fn drop(&mut self) {
                    println!("`Guard` dropped");
                }
            }

            let guard = Guard;

            // `uh_oh` will always raise a Lua error, which will always diverge.
            // However, it is impossible for the compiler to know about this;
            // `guard` may be dropped on some platforms, but that may not be the case on others!
            unsafe {
                lua.push_function(uh_oh);
                lua.managed().call(0, 0);
            }

            drop(guard);
        }));
        unsafe { lua.managed().call(0, 0) }
    })
    .unwrap_err();
}

extern "C-unwind" fn uh_oh(lua: LuaCtx<'_>) -> LuaRets {
    lua.error_c_str(c"uh oh!")
}
