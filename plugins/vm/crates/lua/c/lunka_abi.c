#include <stdarg.h>
#include <stdint.h>

typedef struct lua_State lua_State;
typedef intptr_t lua_KContext;
typedef int (*lua_KFunction)(lua_State *, int, lua_KContext);

extern int barracuda_lua_error_impl(lua_State *state);
extern int barracuda_lua_yieldk_impl(
    lua_State *state,
    int results,
    lua_KContext context,
    lua_KFunction continuation
);
extern int barracuda_luaL_argerror_impl(
    lua_State *state,
    int argument,
    const char *message
);
extern int barracuda_luaL_typeerror_impl(
    lua_State *state,
    int argument,
    const char *type_name
);
extern void luaL_where(lua_State *state, int level);
extern const char *lua_pushvfstring(
    lua_State *state,
    const char *format,
    va_list arguments
);
extern void lua_concat(lua_State *state, int values);

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
    (void)lua_pushvfstring(state, format, arguments);
    va_end(arguments);
    lua_concat(state, 2);
    (void)barracuda_lua_error_impl(state);
    __builtin_unreachable();
}
