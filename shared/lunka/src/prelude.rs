//! Prelude that re-exports useful things, but prepends `Lua` or `lua_` to
//! them to prevent name clashes.

#[cfg(feature = "auxlib")]
pub use crate::{
    AuxOptions as LuaAuxOptions, StaticLibrary as LuaLibrary, cdef::auxlib::luaL_Reg as LuaReg,
    library as lua_library,
};

pub use crate::{
    Coroutine as LuaCoroutine, Ctx as LuaCtx, DebugFlags as LuaDebugFlags, Func as LuaFunc, Lua,
    Rets as LuaRets, Thread as LuaThread,
    cdef::{
        Arith as LuaArith, Compare as LuaCompare, DEFAULT_ID_SIZE as LUA_DEFAULT_ID_SIZE,
        Integer as LuaInteger, Number as LuaNumber, Status as LuaStatus, Type as LuaType,
        Unsigned as LuaUnsigned, lua_Alloc as LuaAlloc, lua_CFunction as LuaCFunction,
        lua_Debug as LuaDebug, lua_KContext as LuaKContext, lua_KFunction as LuaKFunction,
        lua_Reader as LuaReader, lua_WarnFunction as LuaWarnFunction, lua_Writer as LuaWriter,
        lua_upvalueindex as lua_upvalue_index,
    },
    export_fn as lua_export_fn, fmt_error as lua_fmt_error, func as lua_func,
    push_fmt_string as lua_push_fmt_string,
};
