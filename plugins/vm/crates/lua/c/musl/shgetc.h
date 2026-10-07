/*
 * musl's scan helpers, for string input only (see musl's src/internal/shgetc.h
 * and src/internal/shgetc.c, which this follows for a NUL-terminated string
 * with no field width limit).
 */
#include "stdio_impl.h"

#define shcnt(f) ((f)->shcnt + ((f)->rpos - (f)->buf))
#define shlim(f, lim) ((void)((f)->shlim = (lim), (f)->shcnt = (f)->buf - (f)->rpos))
#define shgetc(f) (*(f)->rpos++)
#define shunget(f) ((f)->shlim>=0 ? (void)(f)->rpos-- : (void)0)

#define sh_fromstring(f, s) \
	((f)->buf = (f)->rpos = (void *)(s), (f)->rend = (void*)-1)
