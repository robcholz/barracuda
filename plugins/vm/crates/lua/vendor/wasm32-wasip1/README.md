# Lua 5.4.8 WASI static library

`liblua5.4.a` is built from the Lua 5.4.8 sources bundled by
`lunka-src` 54.8.0 with WASI SDK 33.0. The archive contains the Lua core,
auxiliary library, base library, and package loader used by Barracuda's Lua
runtime. The standalone interpreter and unused OS library are not linked into
the System image. `lunka_abi.c` adapts Lua's integer-returning error and yield
entry points to Lunka's diverging FFI declarations so WebAssembly does not
generate trapping signature-mismatch stubs.

The C sources are compiled for `wasm32-wasip1` with optimization, function and
data sections, WASI signal types, and WebAssembly setjmp/longjmp lowering:

```text
--target=wasm32-wasip1
-D_WASI_EMULATED_SIGNAL
-mllvm -wasm-enable-sjlj
-O2
-std=gnu99
-ffunction-sections
-fdata-sections
```

Rebuild the checked-in archive with:

```text
./rebuild.sh /path/to/wasi-sdk-33.0 /path/to/lua-5.4.8
```

The source is distributed under the Lua license reproduced in `LICENSE`.
The WASI libc, signal, and setjmp archives come from the official WASI SDK
33.0 distribution and retain the licenses and notices of their upstream
`wasi-libc` and LLVM compiler-rt sources. They are checked in so a normal
Browser Platform build does not require clang or a separately installed WASI
SDK.

SHA-256 checksums:

```text
b5871baafa4ae3d73eac78bfa347ef867f057b16b8613f1b4dfcbfe48d53a564  liblua5.4.a
5d8ba34d8c6fd0ac59e0efe37241143887f8f232864bbeacc4181ae739f63371  libc.a
01df54b8698906ed4bb20072c7cc413d5152dcda538f2b1e2b86adfcf5cbd677  libwasi-emulated-signal.a
40eefd1c9715677f95221f3ff6745a1239230c10af4102d82c9258fc843b32d6  libsetjmp.a
```
