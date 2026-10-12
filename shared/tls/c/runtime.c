/*
 * The rest of the C library mbedTLS calls: allocation and console output,
 * forwarded to Rust (src/c_runtime.rs).
 */
#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>

void *barracuda_tls_calloc(size_t count, size_t size);
void barracuda_tls_free(void *pointer);
void barracuda_tls_print(const char *text, size_t length);

void *calloc(size_t count, size_t size)
{
	return barracuda_tls_calloc(count, size);
}

void free(void *pointer)
{
	barracuda_tls_free(pointer);
}

/* Self-test lines are short; longer output is truncated. */
int printf(const char *restrict format, ...)
{
	char text[256];
	va_list arguments;
	int length;

	va_start(arguments, format);
	length = vsnprintf(text, sizeof text, format, arguments);
	va_end(arguments);
	if (length < 0)
		return length;
	barracuda_tls_print(text, (size_t)length < sizeof text ? (size_t)length : sizeof text - 1);
	return length;
}

int puts(const char *text)
{
	size_t length = 0;

	while (text[length])
		length++;
	barracuda_tls_print(text, length);
	return 1;
}
