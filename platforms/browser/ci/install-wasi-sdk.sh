#!/bin/sh
set -eu

work_root=${RUNNER_TEMP:-$(mktemp -d)}
archive="$work_root/wasi-sdk-33.0-x86_64-linux.tar.gz"
sdk="$work_root/wasi-sdk-33.0-x86_64-linux"

curl --fail --location --retry 3 \
    https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-33/wasi-sdk-33.0-x86_64-linux.tar.gz \
    --output "$archive"
echo "0ba8b5bfaeb2adf3f29bab5841d76cf5318ab8e1642ea195f88baba1abd47bce  $archive" \
    | sha256sum --check
tar --extract --gzip --file "$archive" --directory "$work_root"

if [ -n "${GITHUB_ENV:-}" ]; then
    echo "WASI_SDK_PATH=$sdk" >> "$GITHUB_ENV"
else
    printf '%s\n' "$sdk"
fi
