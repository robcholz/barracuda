# Barracuda Rust Framework

## Layout

- `plugins/` contains every system-managed Plugin together with its private
  Component and support crates under that Plugin's `crates/` directory.
- Every Event Router Component integration documents its emitted Events,
  provided RPCs, wire contracts, errors, and lifecycle under its owning
  Plugin's `docs/` directory.
- `shared/` contains crates shared across Plugins and applications.
- `plugins/agent/bench/` contains agent measurement and profiling workloads.
- `core/event-router/bench/profile/` contains Event Router heap/allocation
  profiling workloads.
- `core/event-router/bench/throughput/` contains the Event Router bytes/s
  throughput benchmark and its uv-driven regression pipeline.

## System composition

Barracuda separates portable system behavior from runtime implementations and
physical hardware composition:

- **Platforms** provide implementations of the runtime environment and its
  low-level capabilities, such as executors, networking, storage, and HAL
  access.
- **Drivers** implement concrete peripheral capabilities, such as displays,
  audio devices, cameras, and sensors, behind the traits consumed by the rest
  of the system.
- **Boards** are the hardware composition matrix. A Board selects the exact
  Drivers, buses, pins, and configuration present on one physical product.
  Different Boards may use different peripherals and wiring even when they use
  the same Platform.
- **System** is the `no_std` aggregation layer. It receives low-level platform
  capabilities, constructs the fixed Plugin set, registers every Plugin in
  dependency order, and then starts the complete set. System never selects a
  concrete executor, filesystem, network stack, or listener, and it does not
  construct a Plugin's component-specific services.
- **Plugins** own and load their Components, component-specific runtime
  resources, and other Plugin-scoped resources. Built-in Plugins establish
  their own defaults instead of receiving an assembled component dependency
  bundle from Host. Components and higher-level crates depend on traits and
  portable services, not on a particular Platform, Driver, or Board.

```text
Platform implementations       Peripheral Drivers
            \                       /
             +---- Board matrix ---+
                        |
                     System
                        |
          Plugins [Components + resources]
```

Cross-platform services remain single portable implementations. For example,
`WebServer` is built on picoserve and is shared by Tokio- and Embassy-based
systems. Plugins register endpoints during their lifecycle; the outer
Board/Platform wiring supplies the executor, network stack, listener, and
sockets that drive that same server. Platform selection must not be encoded as
separate `WebServer` implementations or scattered `cfg` branches in portable
system code.

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
[`plugins/agent/crates/trace/scripts/README.md`](plugins/agent/crates/trace/scripts/README.md).

### Context Visualization

```bash
uv run --script plugins/agent/crates/context/scripts/context_viewer.py
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
