/* Lua formats into buffers only, with c/musl/vfprintf.c. */
#ifndef BARRACUDA_LUA_STDIO_H
#define BARRACUDA_LUA_STDIO_H

#include <stddef.h>

#define snprintf barracuda_lua_snprintf

int snprintf(char *restrict buffer, size_t size, const char *restrict format, ...);

#endif
