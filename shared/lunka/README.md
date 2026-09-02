# lunka
Pretty thin bindings to Lua 5.4.

This crate is still a *work-in-progress*.

Please check the latest documentation here:
[Documentation on `docs.rs`](https://docs.rs/lunka/).

# Examples
Creating a Lua "C" library:
```rust
use lunka::prelude::*;

lua_export_fn!(luaopen_hello => |lua| {
	lua.managed().new_lib(&LIBRARY);
	1
});

const LIBRARY: LuaLibrary = library! {
	hello: l_hello,
};

extern "C-unwind" fn l_hello(mut lua: LuaCtx<'_>) -> LuaRets {
	let n = lua.check_number(1);

	lua.managed().push_string("Hello, world!");
	lua.push_number(n * core::f64::consts::PI as LuaNumber);

	LuaRets::new(2)
}
```

For some more examples, check the `examples` directory in the crate's repository.
They are comprehensive enough for actual usage.
