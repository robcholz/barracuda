# Browser WASI runtime archives

These archives come unchanged from WASI SDK 33.0's `wasm32-wasip1` sysroot.
They provide the Browser Platform's C runtime, signal emulation, and setjmp
support. Application crates keep their normal feature sets and link only their
own foreign libraries.

CI downloads the pinned WASI SDK release, verifies its checksum, copies these
files, and checks that the working tree remains unchanged.

The archives are distributed under their upstream `wasi-libc` and LLVM
compiler-rt licenses.

SHA-256 checksums:

```text
5d8ba34d8c6fd0ac59e0efe37241143887f8f232864bbeacc4181ae739f63371  libc.a
40eefd1c9715677f95221f3ff6745a1239230c10af4102d82c9258fc843b32d6  libsetjmp.a
01df54b8698906ed4bb20072c7cc413d5152dcda538f2b1e2b86adfcf5cbd677  libwasi-emulated-signal.a
```
