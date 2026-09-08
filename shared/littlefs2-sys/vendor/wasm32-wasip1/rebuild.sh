#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 /path/to/wasi-sdk" >&2
    exit 2
fi

wasi_sdk_root=$1
script_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
crate_root=$(CDPATH= cd -- "$script_root/../.." && pwd)
build_root=$(mktemp -d)
trap 'rm -rf "$build_root"' EXIT HUP INT TERM

compiler="$wasi_sdk_root/bin/clang"
archiver="$wasi_sdk_root/bin/llvm-ar"
for source_name in lfs lfs_util; do
    "$compiler" \
        --target=wasm32-wasip1 \
        -std=c99 -O2 -ffunction-sections -fdata-sections \
        -DLFS_NO_DEBUG -DLFS_NO_WARN -DLFS_NO_ERROR \
        -DLFS_NO_ASSERT -DLFS_NO_MALLOC -DLFS_MULTIVERSION \
        -I"$crate_root/littlefs" \
        -c "$crate_root/littlefs/$source_name.c" \
        -o "$build_root/$source_name.o"
done

"$archiver" crs "$script_root/liblfs-sys.a" "$build_root"/*.o
shasum -a 256 "$script_root/liblfs-sys.a"
