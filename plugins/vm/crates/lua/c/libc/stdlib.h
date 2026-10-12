/*
 * Lua parses numbers with c/musl/strtod.c and aborts only when an error
 * escapes every protected call; abs is the compiler's.
 */
#ifndef BARRACUDA_LUA_STDLIB_H
#define BARRACUDA_LUA_STDLIB_H

#include <stddef.h>

#define strtod barracuda_lua_strtod
#define abort() __builtin_trap()
#define abs(x) __builtin_abs(x)

double strtod(const char *restrict text, char **restrict end);

#endif
