/*
 * The part of musl's FILE that vsnprintf (vfprintf.c) uses: an output
 * buffer.
 */
#ifndef BARRACUDA_TLS_STDIO_IMPL_H
#define BARRACUDA_TLS_STDIO_IMPL_H

#include <limits.h>
#include <stddef.h>

typedef struct barracuda_tls_file {
	char *buffer;
	size_t size;
	size_t length;
} FILE;

#define ferror(f) 0
/* musl's limit on positional arguments (%1$d) */
#undef NL_ARGMAX
#define NL_ARGMAX 9

#endif
