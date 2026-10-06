/*
 * Lua configuration included at the end of lua.h through LUA_USER_H.
 *
 * Number formatting and parsing go to Rust (src/c_numbers.rs) instead of the
 * C library's snprintf and strtod, which allocate and keep per-thread state.
 * The sandbox never exposes print or locales, so output and the decimal
 * point need no C library either.
 */
#ifndef BARRACUDA_LUA_USER_H
#define BARRACUDA_LUA_USER_H

#include <stddef.h>

int barracuda_lua_format_integer(char *buffer, size_t size, const char *format, long long value);
int barracuda_lua_format_int(char *buffer, size_t size, const char *format, int value);
int barracuda_lua_format_number(char *buffer, size_t size, const char *format, double value);
int barracuda_lua_format_string(char *buffer, size_t size, const char *format, const char *value);
int barracuda_lua_format_pointer(char *buffer, size_t size, const char *format, const void *value);
double barracuda_lua_str2number(const char *text, char **end);

/* Every l_sprintf call passes exactly one argument; pick its formatter. */
#undef l_sprintf
#define l_sprintf(s, sz, f, i) \
	_Generic((i), \
		long long: barracuda_lua_format_integer, \
		int: barracuda_lua_format_int, \
		double: barracuda_lua_format_number, \
		char *: barracuda_lua_format_string, \
		const char *: barracuda_lua_format_string, \
		void *: barracuda_lua_format_pointer, \
		const void *: barracuda_lua_format_pointer)((s), (sz), (f), (i))

#undef lua_str2number
#define lua_str2number(s, p) barracuda_lua_str2number((s), (p))

#undef lua_getlocaledecpoint
#define lua_getlocaledecpoint() '.'

/* string.format('%a') would need the C library's hexadecimal float output. */
#undef lua_number2strx
#define lua_number2strx(L, b, sz, f, n) \
	((void)(b), (void)(sz), (void)(f), (void)(n), \
	 luaL_error((L), "invalid conversion '%%a' to 'format'"))

/* print and warn are not part of the sandbox. */
#define lua_writestring(s, l) ((void)(s), (void)(l))
#define lua_writeline() ((void)0)
#define lua_writestringerror(s, p) ((void)(s), (void)(p))

#endif
