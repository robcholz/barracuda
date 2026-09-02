use lunka::prelude::*;

fn main() {
    let mut lua = Lua::new();
    lua.managed().open_libs();
    assert_eq!(
        lua.managed().load_string(CODE, c"=<embedded>"),
        LuaStatus::Ok,
        "failed to load embedded code"
    );
    let dbg = unsafe { lua.debug() };
    let info = dbg
        .get_func_info(LuaDebugFlags::everything())
        .expect("should be able to get information for our code");
    println!("{info:#?}");
}

const CODE: &str = r#"
-- Example of a script that can be inspected with the debug interface.
print("Hello, world!")
"#;
