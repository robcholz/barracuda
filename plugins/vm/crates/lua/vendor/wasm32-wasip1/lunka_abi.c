#include <stdarg.h>

/* Keep Lua's C ABI under private names; Lunka models these calls as diverging. */
#define lua_error barracuda_lua_error_impl
#define lua_yieldk barracuda_lua_yieldk_impl
#define luaL_argerror barracuda_luaL_argerror_impl
#define luaL_typeerror barracuda_luaL_typeerror_impl
#define luaL_error barracuda_luaL_error_impl
#include "lua.h"
#include "lauxlib.h"
#undef lua_error
#undef lua_yieldk
#undef luaL_argerror
#undef luaL_typeerror
#undef luaL_error

__attribute__((noreturn)) void lua_error(lua_State *state) {
    (void)barracuda_lua_error_impl(state);
    __builtin_unreachable();
}

__attribute__((noreturn)) void lua_yieldk(
    lua_State *state,
    int results,
    lua_KContext context,
    lua_KFunction continuation
) {
    (void)barracuda_lua_yieldk_impl(state, results, context, continuation);
    __builtin_unreachable();
}

__attribute__((noreturn)) void luaL_argerror(
    lua_State *state,
    int argument,
    const char *message
) {
    (void)barracuda_luaL_argerror_impl(state, argument, message);
    __builtin_unreachable();
}

__attribute__((noreturn)) void luaL_typeerror(
    lua_State *state,
    int argument,
    const char *type_name
) {
    (void)barracuda_luaL_typeerror_impl(state, argument, type_name);
    __builtin_unreachable();
}

__attribute__((noreturn)) void luaL_error(
    lua_State *state,
    const char *format,
    ...
) {
    va_list arguments;
    va_start(arguments, format);
    luaL_where(state, 1);
    lua_pushvfstring(state, format, arguments);
    va_end(arguments);
    lua_concat(state, 2);
    (void)barracuda_lua_error_impl(state);
    __builtin_unreachable();
}
