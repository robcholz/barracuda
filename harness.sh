#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

run() {
    printf '\n==> %s\n' "$*"
    "$@"
}

bare_target="riscv32imac-unknown-none-elf"
no_std_crates=(
    barracuda-agent
    barracuda-model-api
    barracuda-agent-context
    barracuda-agent-runtime
    barracuda-fs
    barracuda-net
    barracuda-agent-memory
    barracuda-agent-permission
    barracuda-agent-persistence
    barracuda-agent-sandbox
    barracuda-agent-skill
    barracuda-agent-tool
    barracuda-runtime-utils
    barracuda-lua
    barracuda-vm
    json-validator
)
host_test_crates=(
    barracuda-model-api
    barracuda-agent-context
    barracuda-agent-runtime
    barracuda-fs
    barracuda-net
    barracuda-agent-memory
    barracuda-agent-permission
    barracuda-agent-persistence
    barracuda-agent-sandbox
    barracuda-agent-skill
    barracuda-agent-tool
    barracuda-runtime-utils
    json-validator
    json-validator-macros
)

run cargo fmt --all --check

for crate in "${no_std_crates[@]}"; do
    if [[ "$crate" == "barracuda-agent-runtime" || "$crate" == "barracuda-agent" ]]; then
        continue
    fi
    run cargo check --locked -p "$crate" --target "$bare_target" --no-default-features
done

run cargo check --locked -p barracuda-model-api --target "$bare_target" \
    --no-default-features --features embedded-tls

run cargo check --locked -p barracuda-agent-runtime --target "$bare_target" \
    --no-default-features --features multiagent
run cargo check --locked -p barracuda-agent --target "$bare_target" \
    --no-default-features --features "multiagent cache_profile"

for crate in "${host_test_crates[@]}"; do
    if [[ "$crate" == "barracuda-net" ]]; then
        continue
    fi
    run cargo test --locked -p "$crate"
done
run cargo test --locked -p barracuda-net --features testing
run cargo test --locked -p barracuda-agent-context --features intrusive-observability
run cargo test --locked -p barracuda-agent --test embassy_runtime
run cargo test --locked -p barracuda-cli
run cargo test --locked -p barracuda-lua --features vendored
run cargo test --locked -p barracuda-vm --features vendored

run cargo clippy --locked -p barracuda-agent-runtime --lib -- -D warnings
run cargo clippy --locked -p barracuda-agent --lib -- -D warnings
