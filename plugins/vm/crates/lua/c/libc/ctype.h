/* "C" locale character classes, from c/musl/ctype.c. */
#ifndef BARRACUDA_LUA_CTYPE_H
#define BARRACUDA_LUA_CTYPE_H

#define isalnum barracuda_lua_isalnum
#define isalpha barracuda_lua_isalpha
#define iscntrl barracuda_lua_iscntrl
#define isdigit barracuda_lua_isdigit
#define isgraph barracuda_lua_isgraph
#define islower barracuda_lua_islower
#define isprint barracuda_lua_isprint
#define ispunct barracuda_lua_ispunct
#define isspace barracuda_lua_isspace
#define isupper barracuda_lua_isupper
#define isxdigit barracuda_lua_isxdigit
#define tolower barracuda_lua_tolower
#define toupper barracuda_lua_toupper

int isalnum(int c);
int isalpha(int c);
int iscntrl(int c);
int isdigit(int c);
int isgraph(int c);
int islower(int c);
int isprint(int c);
int ispunct(int c);
int isspace(int c);
int isupper(int c);
int isxdigit(int c);
int tolower(int c);
int toupper(int c);

#endif
