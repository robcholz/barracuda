#!/bin/sh
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 /path/to/wasi-sdk /path/to/lua-5.4.8" >&2
    exit 2
fi

wasi_sdk_root=$1
lua_root=$2
script_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
build_root=$(mktemp -d)
trap 'rm -rf "$build_root"' EXIT HUP INT TERM

lua_source_root="$lua_root/src"
if [ -d "$lua_root/include" ]; then
    lua_include_root="$lua_root/include"
else
    lua_include_root=$lua_source_root
fi

compiler="$wasi_sdk_root/bin/clang"
archiver="$wasi_sdk_root/bin/llvm-ar"
defines="-Dlua_error=barracuda_lua_error_impl -Dlua_yieldk=barracuda_lua_yieldk_impl -DluaL_argerror=barracuda_luaL_argerror_impl -DluaL_typeerror=barracuda_luaL_typeerror_impl -DluaL_error=barracuda_luaL_error_impl"
sources="lapi lauxlib lbaselib lcode lctype ldebug ldo ldump lfunc lgc llex lmem lobject lopcodes lparser lstate lstring ltable ltm lundump lvm lzio loadlib"

for source_name in $sources; do
    # shellcheck disable=SC2086
    "$compiler" \
        --target=wasm32-wasip1 \
        -D_WASI_EMULATED_SIGNAL \
        -mllvm -wasm-enable-sjlj \
        -O2 -std=gnu99 -ffunction-sections -fdata-sections \
        $defines \
        -I"$lua_include_root" \
        -c "$lua_source_root/$source_name.c" \
        -o "$build_root/$source_name.o"
done

# shellcheck disable=SC2086
"$compiler" \
    --target=wasm32-wasip1 \
    -D_WASI_EMULATED_SIGNAL \
    -mllvm -wasm-enable-sjlj \
    -O2 -std=gnu99 -ffunction-sections -fdata-sections \
    $defines \
    -I"$lua_include_root" \
    -c "$script_root/lunka_abi.c" \
    -o "$build_root/lunka_abi.o"

"$archiver" crs "$script_root/liblua5.4.a" "$build_root"/*.o
shasum -a 256 "$script_root/liblua5.4.a"
