# Claw Rust Framework

## Layout

- `crates/` contains production Rust crates.
- `bench/` contains host-only measurement tooling and profiling workloads.

The memory profiler is an executable workload rather than a throughput
benchmark:

```bash
cargo run --profile profiling -p claw-agent-profile -- agent-init
```

## Python tooling

Python tools are members of the root uv workspace and share one `uv.lock` and
one `.venv`. Set up every tool and its development dependencies from this
directory:

```bash
uv sync --all-packages --all-groups
```

Select packaged tools explicitly with `--package`, so member selection never
depends on the current directory. The examples below run from this workspace
root so their input/output paths are also unambiguous. Standalone PEP 723
scripts continue to use `uv run --script`.

## Debugging

### Export Trace

```bash
uv run --package claw-trace \
  claw-trace-chrome <path-to-log> -o <where-you-want-to-emit-chrome-trace>
```

Visualization

[Perfetto UI](https://ui.perfetto.dev/)

The command uses `claw-log`'s canonical Python exporter. Its synthetic Chrome
process/thread mapping (including `run.system`, session grouping, and the
`unattributed` fallback) is documented in
[`crates/claw-log/scripts/README.md`](crates/claw-log/scripts/README.md).

### Context Visualization

```bash
uv run --script crates/claw-context/scripts/context_viewer.py
```

## Embassy integration

Production crates use `no_std + alloc` and do not depend on a chip PAC or a
concrete executor. Constructing an `AgentSystem` also returns an
`AgentService` future. Spawn that future from the application and implement the
three platform traits (`ClawFs`, `ClawHttp`, and `ClawTimer`) with the selected
HAL.

```rust,ignore
let (agent, service) = AgentSystem::new(filesystem, persistence)?;
spawner.spawn(run_agent_service(service))?;
let session = agent.new_session(SessionPersistence::Persistent).await?;
```

The service is a local (`!Send`) future. Use Embassy's current-executor
`Spawner`, which supports non-`Send` tasks; do not use `SendSpawner` for this
task. This is the tradeoff of the allocation-only yield-stream implementation:
no extra generator crate or synchronization, but no cross-executor migration.

Host tests and `claw-cli` remain normal `std` consumers. The former C ABI and
prebuilt static archives are no longer part of this workspace.
