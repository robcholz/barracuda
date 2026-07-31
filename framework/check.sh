#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

# `claw-core` defaults to reasoning_short while the public `claw-agent`
# boundary defaults to reasoning_medium. Building both workspace packages in
# one Cargo invocation unions those mutually exclusive features. Check the
# assembled workspace first, then the implementation crate on its own.
cargo check --workspace --all-targets --exclude claw-core
cargo check -p claw-core --all-targets

if ! cargo public-api --version >/dev/null 2>&1; then
    echo "cargo-public-api is required. Install it with: cargo +stable install cargo-public-api" >&2
    exit 1
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

crates=(
    claw-agent
    claw-api
    claw-cabi
    claw-persistence
    claw-context
    claw-core
    claw-interface
    claw-log
    claw-memory
    claw-permission
    claw-sandbox
    claw-skill
    claw-sys
    claw-tool
    claw-utils
)

for crate in "${crates[@]}"; do
    snapshot="snapshots/${crate}.txt"
    current="${tmpdir}/${crate}.txt"
    echo "checking public API snapshot: ${crate}"
    cargo public-api --manifest-path "crates/${crate}/Cargo.toml" --color never -sss >"${current}"
    diff -u "${snapshot}" "${current}"
done
