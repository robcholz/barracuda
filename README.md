# Barracuda Rust Framework

## Layout

- `plugins/` contains every system-managed Plugin together with its private
  runtime and support crates under that Plugin's `crates/` directory.
- Every Plugin documents its capabilities, Agent Tools, Workflow Actions and
  Events, errors, and lifecycle under its own `docs/` directory.
- `shared/` contains crates shared across Plugins and applications.
- `bench/` contains project-wide measurement and profiling workloads, including
  workload-specific harnesses and shared recording fixtures.
- `tools/` contains project-wide development utilities such as the LLM API
  recorder and deterministic replay proxy.
- `apps/barracuda-system/` owns the portable application lifecycle selected by
  ordinary `cargo run`; the selected Platform supplies only the compile-time
  ABI and executor entry wrapper.
- `apps/barracuda-cli/` is an external terminal Channel that connects to a
  running Gateway; it never constructs the System.

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
  construct a Plugin's private services.
- **Plugins** own their typed capabilities, Workflow integrations, runtime
  tasks, and other Plugin-scoped resources. Built-in Plugins establish their
  own defaults instead of receiving an assembled service bundle from Host.
  Higher-level crates depend on traits and portable services, not on a
  particular concrete Platform.

```text
Embassy entry
     |
Board YAML + Platform YAML
     |
Selected Platform realizes generated Board settings
     |
System application
     |
   System
     |
Plugins [capabilities + Workflow + resources]
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
cargo run
```

The first command opens a colored Board list. Use the arrow keys to move, type
to fuzzy-search, and press Enter to select. It validates the Board bundle and
records the selection in ignored local state. It resolves the matching
self-described Platform, updates the static selected-Platform and
selected-Board-HAL dependency blocks, and writes the target and runner
configuration locally. After that, ordinary `cargo build` and `cargo run`
operate directly on the standalone System application through native Cargo;
there is no Board-aware build wrapper. System, Platform, Core, and Plugin logs
remain in that terminal.

Run the external terminal Channel separately. This command is always compiled
for the development host, even when the selected Board uses an embedded target:

```bash
cargo cli
cargo cli ws://DEVICE_ADDRESS:8787
cargo cli configure
```

Without an explicit URL, the CLI opens a Local/Remote selector. Local displays
and uses the active workspace address published by the macOS launcher, or the
Linux endpoint `ws://10.42.0.2:8787`. Remote prompts for a device IPv4 or IPv6
address and connects to its WebServer on port 8787. Passing an explicit URL
continues to bypass the prompt for scripts and automation.

`cargo cli configure` uses the same Local/Remote selector, then opens the
Barracuda Plugin portal. The portal lists the configuration pages contributed
by the enabled Plugins, so new Plugin configuration surfaces do not require a
new CLI release. Pass an explicit address to skip selection:

```bash
cargo cli configure http://DEVICE_ADDRESS:8787
```

The build defaults Platform logging to `info`. Set `BARRACUDA_LOG_LEVEL` for
one build to select `off`, `error`, `warn`, `info`, `debug`, or `trace`:

```bash
BARRACUDA_LOG_LEVEL=debug cargo run
```

The selected level is validated and baked into the Platform binary; it is not
read from the environment at runtime.

Automation can bypass the prompt with `cargo board select <board-name>`.

When adding a Platform or Board HAL, maintainers update the tracked registries
once and commit the generated blocks:

```bash
cargo platform sync
cargo board sync
```

CI can validate them without writing through the corresponding `--check`
forms. Pulling an up-to-date commit never requires an additional sync step.

Available Board names are the directory names under `boards/configs/`. A
device Board's `toolchain.target` becomes the local Cargo build target; host
Boards omit it.

The memory profiler is an executable workload rather than a throughput
benchmark:

```bash
cargo run --profile profiling -p barracuda-agent-profile -- agent-init
```

## Frontend tooling

Install Bun (CI uses 1.4.0). Frontend tools and dependencies are shared at the
repository root; Plugin source and tests stay in `plugins/<id>/resources/web/`.

```sh
bun install --frozen-lockfile
bun run format          # format frontend source
bun run format:check    # verify formatting without changes
bun run lint
bun run check           # TypeScript types
bun run test            # all frontend tests
```

`cargo build` and `cargo run` automatically build Plugin resources. For
frontend-only work, use `cargo plugin run build`, optionally with
`--plugin <id>`. Each Plugin writes only its own `filesystem/resources/`.
Run the resource build before testing changed frontend source; tests also
verify that committed bundles match their source. `bun run dev` previews the
portal shell locally with an empty manifest, without simulating a device.

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
host Platforms and host tests enable Embassy's native driver and a generic timer queue,
so they exercise the same timing code as firmware.

Each `ModelApi` owns one client created by the shared HTTP client factory,
including its persistent connection and reusable HTTP buffers.
`ModelApiFactory` is only the application construction policy: one call creates
one independent client, so the application decides how many agent clients
exist. Sequential requests on one `ModelApi` reuse its TCP/TLS connection; a
request never reconstructs its client. The shared crate owns reqwless client
construction; Model API owns the LLM-specific request, response, connection,
and stream behavior.

TLS is initialized by the selected Platform and returned beside `ip_stack` in
`PlatformResources`. Linux and macOS load the Host certificate bundle; device
Platforms initialize the same capability from their RNG and DER trust roots.
System passes that capability into Agent construction. No Plugin loads system
certificates or selects a Host-only TLS feature.

Host tests and `barracuda-cli` remain normal `std` consumers. The former C ABI and
prebuilt static archives are no longer part of this workspace.
