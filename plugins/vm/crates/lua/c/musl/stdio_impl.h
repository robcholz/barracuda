/*
 * The part of musl's FILE that snprintf (vfprintf.c) and strtod (strtod.c,
 * floatscan.c) use: an output buffer, or a NUL-terminated input string.
 */
#ifndef BARRACUDA_MUSL_STDIO_IMPL_H
#define BARRACUDA_MUSL_STDIO_IMPL_H

#include <limits.h>
#include <stddef.h>

typedef struct barracuda_lua_file {
	/* snprintf's output */
	char *buffer;
	size_t size;
	size_t length;
	/* strtod's input, read through shgetc.h */
	unsigned char *buf, *rpos, *rend;
	long shlim, shcnt;
} FILE;

#define ferror(f) 0
/* musl's limit on positional arguments (%1$d) */
#undef NL_ARGMAX
#define NL_ARGMAX 9

#endif
