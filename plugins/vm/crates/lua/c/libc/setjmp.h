/*
 * Under GCC Lua jumps with __builtin_setjmp (barracuda_lua_user.h); other
 * compilers use the system's <setjmp.h>.
 */
#ifndef BARRACUDA_LUA_SETJMP_H
#define BARRACUDA_LUA_SETJMP_H

#if !defined(__GNUC__) || defined(__clang__)
#include_next <setjmp.h>
#endif

#endif
