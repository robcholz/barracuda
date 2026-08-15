#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if ! cargo public-api --version >/dev/null 2>&1; then
    echo "cargo-public-api is required. Install it with: cargo +stable install cargo-public-api" >&2
    exit 1
fi

mkdir -p snapshots

crates=(
    barracuda-agent
    barracuda-model-api
    barracuda-agent-persistence
    barracuda-agent-context
    barracuda-agent-runtime
    gateway
    barracuda-fs
    barracuda-net
    barracuda-agent-trace
    barracuda-agent-memory
    barracuda-agent-permission
    barracuda-agent-sandbox
    barracuda-agent-skill
    barracuda-agent-tool
    barracuda-runtime-utils
    barracuda-lua
    barracuda-vm
    http-client
    telegram
    wechat
    bluebubbles
    json-validator
    json-validator-macros
)

manifest_path() {
    case "$1" in
        barracuda-agent) printf "components/agent/crates/agent/Cargo.toml" ;;
        barracuda-model-api) printf "components/agent/crates/model-api/Cargo.toml" ;;
        barracuda-agent-persistence) printf "components/agent/crates/persistence/Cargo.toml" ;;
        barracuda-agent-context) printf "components/agent/crates/context/Cargo.toml" ;;
        barracuda-agent-runtime) printf "components/agent/crates/runtime/Cargo.toml" ;;
        barracuda-agent-trace) printf "components/agent/crates/trace/Cargo.toml" ;;
        barracuda-agent-memory) printf "components/agent/crates/memory/Cargo.toml" ;;
        barracuda-agent-permission) printf "components/agent/crates/permission/Cargo.toml" ;;
        barracuda-agent-sandbox) printf "components/agent/crates/sandbox/Cargo.toml" ;;
        barracuda-agent-skill) printf "components/agent/crates/skill/Cargo.toml" ;;
        barracuda-agent-tool) printf "components/agent/crates/tool/Cargo.toml" ;;
        gateway) printf "components/message-gateway/crates/gateway/Cargo.toml" ;;
        telegram|wechat|bluebubbles) printf "components/message-gateway/crates/%s/Cargo.toml" "$1" ;;
        http-client) printf "shared/http-client/Cargo.toml" ;;
        barracuda-lua) printf "shared/lua/Cargo.toml" ;;
        barracuda-vm) printf "components/vm/Cargo.toml" ;;
        barracuda-fs) printf "shared/fs/Cargo.toml" ;;
        barracuda-net) printf "shared/net/Cargo.toml" ;;
        barracuda-runtime-utils) printf "shared/runtime-utils/Cargo.toml" ;;
        json-validator|json-validator-macros) printf "shared/%s/Cargo.toml" "$1" ;;
        *) echo "unknown crate: $1" >&2; return 1 ;;
    esac
}

for crate in "${crates[@]}"; do
    manifest="$(manifest_path "${crate}")"
    echo "updating public API snapshot: ${crate}"
    cargo public-api --manifest-path "${manifest}" --color never -sss \
        >"snapshots/${crate}.txt"
done
