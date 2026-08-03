# Host profiling

Run commands from the `agent-framework/` directory.

## Agent heap profile

Profile allocations while initializing an `AgentSystem`:

```bash
cargo run --profile profiling -p claw-agent-profile -- agent-init
```

The command prints allocation totals, peak live bytes, and retained bytes. Its
DHAT report is written to:

```text
target/profiles/agent-init.dhat.json
```

Pass a second argument to choose another output path:

```bash
cargo run --profile profiling -p claw-agent-profile -- \
  agent-init target/profiles/custom.dhat.json
```

`claw-profile` is the shared profiling library; it has no standalone command.

## LLM API byte recording and replay

[`llm-tape`](llm-tape/README.md) is an independent Python reverse proxy for
recording uninterpreted LLM HTTP response chunks and replaying them with their
original timing. It is intended for deterministic agent trajectory benchmarks;
it does not parse SSE or model output.

## Binary size

Build the host profiling executable and print its load-image breakdown:

```bash
cargo build --profile profiling -p claw-agent-profile
size target/profiling/claw-agent-profile
```

In the output, `dec` is `text + data + bss`. Use `size -A` for individual ELF
sections. The command does not calculate a delta against an older build.

## Stack usage

Run Clippy's static stack-frame estimate with the 4 KiB threshold configured in
`bench/clippy.toml`:

```bash
CLIPPY_CONF_DIR=bench cargo clippy -p claw-agent-profile --all-targets -- \
  -W clippy::large-stack-frames -W clippy::large-stack-arrays
```

Clippy reports functions estimated to exceed the threshold. These estimates are
useful for regression checks but may differ from optimized runtime stack usage.

## Bounded runtime stack

Run agent initialization with the runtime worker stack size requested by
`claw-core` (currently 64 KiB):

```bash
cargo run --profile profiling -p claw-agent-profile --bin claw-agent-stack
```

The command exits successfully only if the `agent-init` workload completes
within that worker stack. This binary does not link DHAT, so allocator
backtraces do not inflate its stack use. It is a host smoke test; device
high-water-mark measurements remain authoritative.
