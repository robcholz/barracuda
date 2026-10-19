#!/usr/bin/env bash
# Builds Live Editor on a plain build machine, such as Vercel's, and copies it to a site's
# directory. The website's build runs it so the site serves Live Editor at /editor/:
#
#   bash ../apps/live-editor/web/build-site.sh public/editor   (from website/)
#
# It installs what is missing: Rust 1.98.1 with its WebAssembly target, Bun 1.4.0, and PocketJS
# at the commit docs/plugins/app-manager/pocketjs.mdx names.
set -euo pipefail

POCKETJS_COMMIT=e9e417308f094ead706754164acf91436e4d05a8
RUST_VERSION=1.98.1
BUN_VERSION=1.4.0

out="$(mkdir -p "$1" && cd "$1" && pwd)"
web="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$web/../../.." && pwd)"
cache="${LIVE_EDITOR_CACHE:-$repo/.live-editor-build}"
mkdir -p "$cache"

if ! command -v cargo >/dev/null 2>&1; then
  [ -f "$HOME/.cargo/env" ] || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
    sh -s -- -y --profile minimal --default-toolchain "$RUST_VERSION" --no-modify-path
  # shellcheck source=/dev/null
  . "$HOME/.cargo/env"
fi
rustup target add wasm32-unknown-unknown --toolchain "$RUST_VERSION"

if [ "$(bun --version 2>/dev/null || true)" != "$BUN_VERSION" ]; then
  curl -fsSL https://bun.sh/install | bash -s "bun-v$BUN_VERSION" >/dev/null
  export PATH="$HOME/.bun/bin:$PATH"
fi

pocketjs="$cache/pocketjs-$POCKETJS_COMMIT"
if [ ! -f "$pocketjs/package.json" ]; then
  rm -rf "$pocketjs"
  if command -v git >/dev/null 2>&1; then
    git init -q "$pocketjs"
    git -C "$pocketjs" fetch -q --depth 1 https://github.com/pocket-nexus/pocketjs "$POCKETJS_COMMIT"
    git -C "$pocketjs" checkout -q FETCH_HEAD
  else
    curl -fsSL "https://codeload.github.com/pocket-nexus/pocketjs/tar.gz/$POCKETJS_COMMIT" | tar -xz -C "$cache"
  fi
fi
(cd "$pocketjs" && bun install --frozen-lockfile)
(cd "$repo" && bun install --frozen-lockfile)

POCKETJS_ROOT="$pocketjs" CARGO_TARGET_DIR="$cache/target" bun "$web/build.ts"

rm -rf "$out"
mkdir -p "$out"
cp -R "$web/dist/." "$out/"
# Source maps stay out of the published site.
find "$out" -name '*.map' -delete
echo "Live Editor: $out"
