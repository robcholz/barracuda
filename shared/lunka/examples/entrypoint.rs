use lunka::prelude::*;

lua_export_fn!(luaopen_entrypoint => |lua| {
    lua.check_version();
    lua.managed().push_c_str(c"Hello, world!");
    1
});
