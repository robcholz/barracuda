/*
 * Barracuda's Lua configuration, included at the end of lua.h through
 * LUA_USER_H.
 *
 * Lua here links no C library. The headers in libc/ point the C functions it
 * calls at the copies in musl/, and the patches in ../patches remove files,
 * processes, the clock, printing and the C heap. Every target builds the same
 * Lua.
 */
#ifndef BARRACUDA_LUA_USER_H
#define BARRACUDA_LUA_USER_H

/* Numbers always use '.', as in the "C" locale. */
#undef lua_getlocaledecpoint
#define lua_getlocaledecpoint() '.'

/*
 * Fixed seeds for string hashes and for the pivots table.sort picks after an
 * unbalanced partition. Neither changes results; random seeds would only
 * resist inputs crafted to be slow, and each VM's memory already bounds that.
 */
#define luai_makeseed(L) ((void)(L), 0u)
#define l_randomizePivot() (~0u)

/*
 * Lua raises errors with a non-local jump. GCC, which compiles every
 * bare-metal target, provides one without the C library; other compilers use
 * <setjmp.h>.
 */
#if defined(__GNUC__) && !defined(__clang__)
typedef void *barracuda_lua_jmpbuf[5];
#define luai_jmpbuf barracuda_lua_jmpbuf
#define LUAI_THROW(L, c) __builtin_longjmp((c)->b, 1)
#define LUAI_TRY(L, c, a) if (__builtin_setjmp((c)->b) == 0) { a }
#endif

#endif
