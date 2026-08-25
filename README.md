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
platform implementations:

- **Boards** live under `boards/`. Their YAML files contain product-fixed
  flash layout and storage mappings; the Board config crate validates and
  generates a static, platform-neutral description at build time.
- **Platform contract** lives in `platforms/api`. It defines the
  compile-time initialization boundary and the resources consumed by System.
- **Concrete Platforms** live under `platforms/<name>`. Host and device
  Platforms use the same Embassy executor, task, timer, and lifecycle model;
  only their low-level network, filesystem, flash, and HAL adapters differ.
- **Board and Platform are selected independently** by build configuration.
  A Board Rust type never selects or depends on a Platform type.
- **System** is the `no_std` aggregation layer. It receives low-level platform
  capabilities, constructs the fixed Plugin set, registers every Plugin in
  dependency order, and then starts the complete set. System and Plugins never
  select a host/device executor, filesystem, network stack, or listener and do not
  construct a Plugin's component-specific services.
- **Plugins** own and load their Components, component-specific runtime
  resources, and other Plugin-scoped resources. Built-in Plugins establish
  their own defaults instead of receiving an assembled component dependency
  bundle from Host. Components and higher-level crates depend on traits and
  portable services, not on a particular concrete Platform.

```text
Embassy entry
     |
Board YAML + Platform YAML
     |
Selected Platform realizes generated Board settings
     |
   System
     |
Plugins [Components + resources]
```

Cross-platform services remain single portable implementations. For example,
`WebServer` is built on picoserve and is shared by host and device Platforms.
Plugins register endpoints during their lifecycle; the selected Platform
supplies the network stack, listener, and sockets that drive that same server.
Platform selection must not be encoded as separate `WebServer`
implementations, scattered `cfg` branches, or a different application executor
in portable system code.

## Select and build a Board

Board selection is a persistent workspace action, separate from compilation:

```bash
cargo board select
cargo build
```

The first command opens a colored Board list. Use the arrow keys to move, type
to fuzzy-search, and press Enter to select. It validates the Board bundle and
records the selection in ignored local state at
`.barracuda/selected-board`. The second command is the ordinary Cargo build;
no `BARRACUDA_BOARD` environment variable or custom build wrapper is required.
The Rust target independently selects the Platform, and compilation rejects an
incompatible Board/Platform pair.

Automation can bypass the prompt with `cargo board select <board-name>`.

Available Board names are the directory names under `boards/configs/`.
Cross-compilation continues to use Cargo's normal `--target` and `-p`
arguments; selecting a Board does not rewrite Cargo's target configuration.

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
`RuntimeService` future. Spawn that future from the application, supply the
Plugin's System-scoped `Vfs`, and construct the model API from Embassy TCP and
DNS resources.

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

Each `ModelApi` owns one client from `shared/http-client`, including its
persistent connection and reusable HTTP buffers. `ModelApiFactory` is only the
application construction policy: one call creates one independent client, so
the application decides how many agent clients exist. Sequential requests on
one `ModelApi` reuse its TCP/TLS connection; a request never reconstructs its
client. Model API contains only the LLM-specific request, response, and stream
adapter; reqwless integration lives exclusively in `shared/http-client`.

TLS is initialized by the selected Platform and returned beside `ip_stack` in
`PlatformResources`. Linux and macOS load the Host certificate bundle; device
Platforms initialize the same capability from their RNG and DER trust roots.
System passes that capability into Agent construction. No Plugin loads system
certificates or selects a Host-only TLS feature.

Host tests and `barracuda-cli` remain normal `std` consumers. The former C ABI and
prebuilt static archives are no longer part of this workspace.
