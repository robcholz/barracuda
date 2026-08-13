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
    gateway
    claw-fs
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

manifest_path() {
    case "$1" in
        gateway)
            printf 'components/message-gateway/crates/gateway/Cargo.toml'
            ;;
        claw-fs|claw-net|claw-utils|json-validator|json-validator-macros)
            printf 'shared/%s/Cargo.toml' "$1"
            ;;
        *)
            printf 'components/agent/crates/%s/Cargo.toml' "$1"
            ;;
    esac
}

for crate in "${crates[@]}"; do
    snapshot="snapshots/${crate}.txt"
    current="${tmpdir}/${crate}.txt"
    manifest="$(manifest_path "${crate}")"
    echo "checking public API snapshot: ${crate}"
    cargo public-api --manifest-path "${manifest}" --color never -sss >"${current}"
    diff -u "${snapshot}" "${current}"
done
