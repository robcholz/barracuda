# littlefs2-sys WASI boundary

This fork makes the ordinary `littlefs2-sys` feature set build on WebAssembly.
Native targets retain the upstream C and bindgen build. On wasm32, the crate's
build script uses checked-in bindings and compiles the selected upstream C
sources with WASI SDK, so the same `littlefs2` dependency and `c-stubs`
behavior are used on every Platform.

The WASI objects are built from the vendored upstream `littlefs/lfs.c` and
`littlefs/lfs_util.c` (or the upstream patched source selected by the existing
feature) with WASI SDK and Barracuda's resolved feature set:

```text
--target=wasm32-wasip1
-std=c99
-DLFS_NO_DEBUG
-DLFS_NO_WARN
-DLFS_NO_ERROR
-DLFS_NO_ASSERT
-DLFS_NO_MALLOC
-DLFS_MULTIVERSION
```

The committed bindings preserve wasm32 layouts and explicitly declare the C
functions. Bindgen 0.70 does not recognize Clang 22's WebAssembly calling
convention and otherwise silently omits those declarations.
