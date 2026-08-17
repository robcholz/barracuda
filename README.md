# Barracuda Rust Framework

## Layout

- `components/` contains event-router components; a component may own multiple crates.
- `shared/` contains crates shared across components and applications.
- `components/agent/bench/` contains agent measurement and profiling workloads.
- `core/event-router/bench/profile/` contains Event Router heap/allocation
  profiling workloads.
- `core/event-router/bench/throughput/` contains the Event Router bytes/s
  throughput benchmark and its uv-driven regression pipeline.

The memory profiler is an executable workload rather than a throughput
benchmark:

```bash
cargo run --profile profiling -p barracuda-agent-profile -- agent-init
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
uv run --package barracuda-trace \
  barracuda-trace-chrome <path-to-log> -o <where-you-want-to-emit-chrome-trace>
```

Visualization

[Perfetto UI](https://ui.perfetto.dev/)

The command uses `barracuda-agent-trace`'s canonical Python exporter. Its synthetic Chrome
process/thread mapping (including `run.system`, session grouping, and the
`unattributed` fallback) is documented in
[`components/agent/crates/trace/scripts/README.md`](components/agent/crates/trace/scripts/README.md).

### Context Visualization

```bash
uv run --script components/agent/crates/context/scripts/context_viewer.py
```

## Embassy integration

Production crates use `no_std + alloc` and do not depend on a chip PAC or a
concrete executor. Constructing an `AgentRuntime` also returns an
`RuntimeService` future. Spawn that future from the application, implement
`FileSystem`, and provide an `embedded-nal-async` TCP/DNS stack to `barracuda-net`.

```rust,ignore
let factory = ModelApiFactory::new(|| build_barracuda_model_api_from_static_resources());
let (agent, service) = AgentRuntime::new(filesystem, persistence, factory)?;
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

Each `ModelApi` exclusively owns one long-lived reqwless client, its persistent
`HttpResource`, and reusable HTTP buffers. `ModelApiFactory` is only the
application construction policy: one call creates one independent client, so
the application decides how many agent clients exist. Sequential requests on
one `ModelApi` reuse its TCP/TLS connection; a request never reconstructs its
client.

For portable no_std HTTPS, enable `barracuda-model-api/embedded-tls` and construct `ModelApi`
with `TlsVerify::Certificate`; the application provides the static TLS buffers,
random seed, and DER CA certificate. Platforms with mbedTLS can instead enable
`barracuda-model-api/mbedtls`. The host CLI uses `barracuda-model-api/mbedtls-host`, a Tokio TCP/DNS
HAL, and a PEM CA bundle, but its HTTP request path is still the same reqwless
client.

Host tests and `barracuda-cli` remain normal `std` consumers. The former C ABI and
prebuilt static archives are no longer part of this workspace.
