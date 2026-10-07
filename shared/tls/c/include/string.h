/* The string functions mbedTLS and the copied musl call. */
#ifndef BARRACUDA_TLS_STRING_H
#define BARRACUDA_TLS_STRING_H

#include <stddef.h>

/* The compiler runtime provides these on every target. */
void *memcpy(void *restrict destination, const void *restrict source, size_t length);
void *memset(void *destination, int byte, size_t length);
int memcmp(const void *left, const void *right, size_t length);
size_t strlen(const char *string);

void *memchr(const void *bytes, int byte, size_t length);
int strcmp(const char *left, const char *right);
char *strstr(const char *haystack, const char *needle);
char *strchr(const char *string, int byte);
char *__strchrnul(const char *string, int byte);

#endif
