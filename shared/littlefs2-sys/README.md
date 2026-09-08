# littlefs2-sys WASI boundary

This is a narrow fork of `littlefs2-sys` 0.4.0. Native targets retain the
upstream C and bindgen build. `wasm32-wasip1` uses checked-in bindings and a
static archive so selecting the Browser Board does not require a local C
toolchain.

The WASI archive is built from the unmodified upstream `littlefs/lfs.c` and
`littlefs/lfs_util.c` with WASI SDK 33.0 and Barracuda's resolved feature set:

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

Rebuild the checked-in archive with:

```text
./vendor/wasm32-wasip1/rebuild.sh /path/to/wasi-sdk-33.0
```

SHA-256:

- `liblfs-sys.a`: `86f6d6e6e073397b2519007d765b16ab027098947e67698e2aa89c9980fe1b91`

The committed bindings preserve wasm32 layouts and explicitly declare the C
functions. Bindgen 0.70 does not recognize Clang 22's WebAssembly calling
convention and otherwise silently omits those declarations.
