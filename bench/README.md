# Project profiling and deterministic workloads

Run commands from the repository root.

## Agent heap profile

Profile allocations while initializing an `AgentRuntime`:

```bash
cargo run --profile profiling -p barracuda-agent-profile -- agent-init
```

The command prints allocation totals, peak live bytes, and retained bytes. Its
DHAT report is written to:

```text
target/profiles/agent-init.dhat.json
```

Pass a second argument to choose another output path:

```bash
cargo run --profile profiling -p barracuda-agent-profile -- \
  agent-init target/profiles/custom.dhat.json
```

Replay every recorded response in the long `overall-capabilities` tape through
the production OpenAI-compatible streaming parser under DHAT:

```bash
cargo run --profile profiling -p barracuda-agent-profile -- tape-replay
```

This scenario deliberately uses seven-byte network reads to expose allocation
churn at chunk/parser boundaries. The report is written to
`target/profiles/tape-replay.dhat.json`. Open it in DHAT's viewer and sort
allocation points by **Total blocks** to find frequent allocate/free sites, or
by **Total bytes** and **At t-gmax** to find high-volume and peak-live sites.
The latest checked analysis is in [`results/tape-replay.md`](results/tape-replay.md).

[`barracuda-profile`](../shared/profile/) is the shared profiling library;
it has no standalone command and is not owned by the Agent component.

## LLM API byte recording and replay

[`llm-tape`](../tools/llm-tape/README.md) is an independent Python reverse proxy for
recording uninterpreted LLM HTTP response chunks and replaying them with their
original timing. It is intended for deterministic agent trajectory benchmarks;
it does not parse SSE or model output.

## Binary size

Build the host profiling executable and print its load-image breakdown:

```bash
cargo build --profile profiling -p barracuda-agent-profile
size target/profiling/barracuda-agent-profile
```

In the output, `dec` is `text + data + bss`. Use `size -A` for individual ELF
sections. The command does not calculate a delta against an older build.

## Stack usage

Run Clippy's static stack-frame estimate with the 4 KiB threshold configured in
`bench/clippy.toml`:

```bash
CLIPPY_CONF_DIR=bench cargo clippy -p barracuda-agent-profile --all-targets -- \
  -W clippy::large-stack-frames -W clippy::large-stack-arrays
```

Clippy reports functions estimated to exceed the threshold. These estimates are
useful for regression checks but may differ from optimized runtime stack usage.

## Bounded runtime stack

Run agent initialization with the runtime worker stack size requested by
`barracuda-agent-runtime` (currently 64 KiB):

```bash
cargo run --profile profiling -p barracuda-agent-profile --bin barracuda-agent-stack
```

The command exits successfully only if the `agent-init` workload completes
within that worker stack. This binary does not link DHAT, so allocator
backtraces do not inflate its stack use. It is a host smoke test; device
high-water-mark measurements remain authoritative.
