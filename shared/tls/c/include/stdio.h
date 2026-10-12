/* mbedTLS formats into buffers, and prints its self-test through printf. */
#ifndef BARRACUDA_TLS_STDIO_H
#define BARRACUDA_TLS_STDIO_H

#include <stdarg.h>
#include <stddef.h>

int vsnprintf(char *restrict buffer, size_t size, const char *restrict format, va_list arguments);
int snprintf(char *restrict buffer, size_t size, const char *restrict format, ...);
int printf(const char *restrict format, ...);
int puts(const char *text);

#endif
