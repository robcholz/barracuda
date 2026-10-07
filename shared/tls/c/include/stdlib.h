/* The allocator mbedTLS calls by default. */
#ifndef BARRACUDA_TLS_STDLIB_H
#define BARRACUDA_TLS_STDLIB_H

#include <stddef.h>

void *calloc(size_t count, size_t size);
void free(void *pointer);

#endif
