/* The string functions mbedTLS and the copied musl call. */
#ifndef BARRACUDA_TLS_STRING_H
#define BARRACUDA_TLS_STRING_H

#include <stddef.h>

/* The compiler runtime provides these on every target. */
void *memcpy(void *restrict destination, const void *restrict source, size_t length);
void *memset(void *destination, int byte, size_t length);

void *memchr(const void *bytes, int byte, size_t length);
int strcmp(const char *left, const char *right);

#endif
