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
`AgentService` future. Spawn that future from the application, implement
`ClawFs`, and provide an `embedded-nal-async` TCP/DNS stack to `claw-net`.

```rust,ignore
let factory = ClawApiFactory::new(|| build_claw_api_from_static_resources());
let (agent, service) = AgentSystem::new(filesystem, persistence, factory)?;
spawner.spawn(run_agent_service(service))?;
let session = agent.new_session(SessionPersistence::Persistent).await?;
```

The service is a local (`!Send`) future. Use Embassy's current-executor
`Spawner`, which supports non-`Send` tasks; do not use `SendSpawner` for this
task. This is the tradeoff of the allocation-only yield-stream implementation:
no extra generator crate or synchronization, but no cross-executor migration.

Deadlines, retry backoff, and orchestration timeouts use `embassy-time`
directly. A firmware application supplies the one global Embassy time driver
through its HAL; it does not implement a framework-specific timer trait. The
host CLI and host tests enable Embassy's `std` driver and a generic timer queue,
so they exercise the same timing code as firmware.

Each `ClawApi` exclusively owns one long-lived reqwless client, its persistent
`HttpResource`, and reusable HTTP buffers. `ClawApiFactory` is only the
application construction policy: one call creates one independent client, so
the application decides how many agent clients exist. Sequential requests on
one `ClawApi` reuse its TCP/TLS connection; a request never reconstructs its
client.

For portable no_std HTTPS, enable `claw-api/embedded-tls` and construct `ClawApi`
with `TlsVerify::Certificate`; the application provides the static TLS buffers,
random seed, and DER CA certificate. Platforms with mbedTLS can instead enable
`claw-api/mbedtls`. The host CLI uses `claw-api/mbedtls-host`, a Tokio TCP/DNS
HAL, and a PEM CA bundle, but its HTTP request path is still the same reqwless
client.

Host tests and `claw-cli` remain normal `std` consumers. The former C ABI and
prebuilt static archives are no longer part of this workspace.
