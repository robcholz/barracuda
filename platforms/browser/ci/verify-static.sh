#!/bin/sh
set -eu

work_root=${RUNNER_TEMP:-$(mktemp -d)}
wasi_archive="$work_root/wasi-sdk-33.0-x86_64-linux.tar.gz"
wasi_root="$work_root/wasi-sdk-33.0-x86_64-linux"
lua_archive="$work_root/lua-5.4.8.tar.gz"
lua_root="$work_root/lua-5.4.8"

curl --fail --location --retry 3 \
    https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-33/wasi-sdk-33.0-x86_64-linux.tar.gz \
    --output "$wasi_archive"
echo "0ba8b5bfaeb2adf3f29bab5841d76cf5318ab8e1642ea195f88baba1abd47bce  $wasi_archive" \
    | sha256sum --check
tar --extract --gzip --file "$wasi_archive" --directory "$work_root"

curl --fail --location --retry 3 \
    https://www.lua.org/ftp/lua-5.4.8.tar.gz \
    --output "$lua_archive"
echo "4f18ddae154e793e46eeab727c59ef1c0c0c2b744e7b94219710d76f530629ae  $lua_archive" \
    | sha256sum --check
tar --extract --gzip --file "$lua_archive" --directory "$work_root"

plugins/vm/crates/lua/vendor/wasm32-wasip1/rebuild.sh "$wasi_root" "$lua_root"
shared/littlefs2-sys/vendor/wasm32-wasip1/rebuild.sh "$wasi_root"

wasi_lib="$wasi_root/share/wasi-sysroot/lib/wasm32-wasip1"
cp "$wasi_lib/libc.a" platforms/browser/vendor/wasi/libc.a
cp "$wasi_lib/libsetjmp.a" platforms/browser/vendor/wasi/libsetjmp.a
cp "$wasi_lib/libwasi-emulated-signal.a" \
    platforms/browser/vendor/wasi/libwasi-emulated-signal.a
git diff --exit-code -- \
    plugins/vm/crates/lua/vendor/wasm32-wasip1 \
    platforms/browser/vendor/wasi \
    shared/littlefs2-sys/vendor/wasm32-wasip1
