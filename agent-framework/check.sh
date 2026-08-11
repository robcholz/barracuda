#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

cargo check --workspace
cargo check -p claw-agent --target riscv32imac-unknown-none-elf --features multiagent

if ! cargo public-api --version >/dev/null 2>&1; then
    echo "cargo-public-api is required. Install it with: cargo +stable install cargo-public-api" >&2
    exit 1
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

crates=(
    claw-agent
    claw-api
    claw-persistence
    claw-context
    claw-core
    claw-interface
    claw-net
    claw-log
    claw-memory
    claw-permission
    claw-sandbox
    claw-skill
    claw-tool
    claw-utils
    json-validator
    json-validator-macros
)

for crate in "${crates[@]}"; do
    snapshot="snapshots/${crate}.txt"
    current="${tmpdir}/${crate}.txt"
    echo "checking public API snapshot: ${crate}"
    cargo public-api --manifest-path "crates/${crate}/Cargo.toml" --color never -sss >"${current}"
    diff -u "${snapshot}" "${current}"
done
