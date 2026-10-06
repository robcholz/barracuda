/*
 * The string functions Lua (and the copied musl) call, from c/musl. Their
 * names get a barracuda_lua_ prefix so they never meet another C library.
 */
#ifndef BARRACUDA_LUA_STRING_H
#define BARRACUDA_LUA_STRING_H

#include <stddef.h>

/* The compiler runtime provides these on every target. */
void *memcpy(void *restrict destination, const void *restrict source, size_t length);
void *memmove(void *destination, const void *source, size_t length);
void *memset(void *destination, int byte, size_t length);
int memcmp(const void *left, const void *right, size_t length);

#define memchr barracuda_lua_memchr
#define strlen barracuda_lua_strlen
#define strnlen barracuda_lua_strnlen
#define strcmp barracuda_lua_strcmp
#define strncmp barracuda_lua_strncmp
/* Strings compare by bytes, as in the "C" locale. */
#define strcoll barracuda_lua_strcmp
#define strchr barracuda_lua_strchr
#define strpbrk barracuda_lua_strpbrk
#define strspn barracuda_lua_strspn
#define strcspn barracuda_lua_strcspn
#define strstr barracuda_lua_strstr
#define strcpy barracuda_lua_strcpy

void *memchr(const void *bytes, int byte, size_t length);
size_t strlen(const char *text);
size_t strnlen(const char *text, size_t limit);
int strcmp(const char *left, const char *right);
int strncmp(const char *left, const char *right, size_t length);
char *strchr(const char *text, int byte);
char *strpbrk(const char *text, const char *set);
size_t strspn(const char *text, const char *set);
size_t strcspn(const char *text, const char *set);
char *strstr(const char *text, const char *pattern);
char *strcpy(char *restrict destination, const char *restrict source);

#endif
