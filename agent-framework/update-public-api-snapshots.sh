#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if ! cargo public-api --version >/dev/null 2>&1; then
    echo "cargo-public-api is required. Install it with: cargo +stable install cargo-public-api" >&2
    exit 1
fi

mkdir -p snapshots

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
    claw-lua
    claw-vm
    http-client
    telegram
    wechat
    bluebubbles
    json-validator
    json-validator-macros
)

manifest_path() {
    case "$1" in
        gateway)
            printf 'components/message-gateway/crates/gateway/Cargo.toml'
            ;;
        telegram|wechat|bluebubbles)
            printf 'components/message-gateway/crates/%s/Cargo.toml' "$1"
            ;;
        http-client)
            printf 'shared/http-client/Cargo.toml'
            ;;
        claw-lua)
            printf 'shared/lua/Cargo.toml'
            ;;
        claw-vm)
            printf 'components/vm/Cargo.toml'
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
    manifest="$(manifest_path "${crate}")"
    echo "updating public API snapshot: ${crate}"
    cargo public-api --manifest-path "${manifest}" --color never -sss \
        >"snapshots/${crate}.txt"
done
