# musl sources for Lua

The C library functions Lua calls, copied from
[musl](https://musl.libc.org/) 1.2.5 so that Lua links no C library on any
target. `COPYRIGHT` is musl's license (MIT). `build.rs` compiles these files
into `liblua_musl.a` with `barracuda_musl.h` included first, and `../libc/`
maps each name Lua uses to its `barracuda_lua_` copy.

| Files | musl source | Changes |
| --- | --- | --- |
| `floor.c`, `fmod.c`, `frexp.c`, `ldexp.c`, `pow.c`, `pow_data.[ch]`, `exp_data.[ch]`, `scalbn.c`, `copysign.c`, `__math_{invalid,oflow,uflow,xflow}.c` | `src/math` | `#include <features.h>` removed from the two data headers |
| `libm.h` | `src/internal/libm.h` | reduced to what these files use: no `long double` layouts or helpers, no declarations of functions not copied |
| `memchr.c`, `strchr.c`, `strchrnul.c`, `strcmp.c`, `strcpy.c`, `stpcpy.c`, `strcspn.c`, `strlen.c`, `strncmp.c`, `strnlen.c`, `strpbrk.c`, `strspn.c`, `strstr.c` | `src/string` | none |
| `ctype.c` | `src/ctype` | one file with the "C" locale functions, without `locale_t` variants and aliases |
| `floatscan.[ch]`, `strtod.c` | `src/internal`, `src/stdlib` | `long double` is `double`, as in musl on targets where they are the same; `errno` writes removed; only `strtod` kept |
| `vfprintf.c` | `src/stdio` | becomes `snprintf` over a buffer; `long double` is `double`; no `%m`, `%C`, `%S`, `%L` or `errno` |
| `stdio_impl.h`, `shgetc.h` | `src/internal` | rewritten: the part of `FILE` that `snprintf` and `strtod` use, for a buffer or a string |
| `barracuda_musl.h` | — | Barracuda's: prefixes musl's internal global names and drops its visibility and alias markers |
