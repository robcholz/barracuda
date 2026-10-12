# C library for mbedTLS

mbedTLS calls a few C library functions. Barracuda links no C library on
bare-metal targets, so `build.rs` compiles these files into
`libtls_c_runtime.a` on those targets only. `weak.h` is included first and
makes every function weak: a definition the target already links (chip ROM,
the allocator's `calloc`/`free`, a radio library's `printf`) takes
precedence, and these fill only what is missing. Host targets use their
system C library and compile none of this.

| Files | Source | Changes |
| --- | --- | --- |
| `musl/memchr.c`, `musl/strchr.c`, `musl/strchrnul.c`, `musl/strcmp.c`, `musl/strstr.c` | [musl](https://musl.libc.org/) 1.2.5 `src/string` | none |
| `musl/vfprintf.c` | musl 1.2.5 `src/stdio`, as already reduced in `plugins/vm/crates/lua/c/musl` | becomes `vsnprintf` and `snprintf` over a buffer; no floating-point conversions (mbedTLS formats none, so `%e`, `%f`, `%g` and `%a` are rejected); a private `strnlen` |
| `musl/stdio_impl.h` | musl `src/internal` | rewritten: the part of `FILE` that `vsnprintf` uses |
| `musl/COPYRIGHT` | musl | musl's license (MIT) |
| `include/*.h` | — | Barracuda's: declarations of only what these files and mbedTLS use |
| `runtime.c` | — | Barracuda's: `calloc`, `free`, `printf` and `puts` over Rust (`src/c_runtime.rs`) |
| `weak.h` | — | Barracuda's: weak definitions; drops musl's visibility and alias markers |
