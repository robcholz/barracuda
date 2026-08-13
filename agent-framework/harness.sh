#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

run() {
    printf '\n==> %s\n' "$*"
    "$@"
}

bare_target="riscv32imac-unknown-none-elf"
no_std_crates=(
    claw-agent
    claw-api
    claw-context
    claw-core
    claw-fs
    claw-net
    claw-memory
    claw-permission
    claw-persistence
    claw-sandbox
    claw-skill
    claw-tool
    claw-utils
    json-validator
)
host_test_crates=(
    claw-api
    claw-context
    claw-core
    claw-fs
    claw-net
    claw-memory
    claw-permission
    claw-persistence
    claw-sandbox
    claw-skill
    claw-tool
    claw-utils
    json-validator
    json-validator-macros
)

run cargo fmt --all --check

for crate in "${no_std_crates[@]}"; do
    if [[ "$crate" == "claw-core" || "$crate" == "claw-agent" ]]; then
        continue
    fi
    run cargo check --locked -p "$crate" --target "$bare_target" --no-default-features
done

run cargo check --locked -p claw-api --target "$bare_target" \
    --no-default-features --features embedded-tls

run cargo check --locked -p claw-core --target "$bare_target" \
    --no-default-features --features multiagent
run cargo check --locked -p claw-agent --target "$bare_target" \
    --no-default-features --features "multiagent cache_profile"

for crate in "${host_test_crates[@]}"; do
    if [[ "$crate" == "claw-net" ]]; then
        continue
    fi
    run cargo test --locked -p "$crate"
done
run cargo test --locked -p claw-net --features testing
run cargo test --locked -p claw-context --features intrusive-observability
run cargo test --locked -p claw-agent --test embassy_runtime
run cargo test --locked -p claw-cli

run cargo clippy --locked -p claw-core --lib -- -D warnings
run cargo clippy --locked -p claw-agent --lib -- -D warnings
