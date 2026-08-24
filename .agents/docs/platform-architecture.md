# Platform Architecture

This document is the authoritative target architecture for Barracuda platform
integration. It takes precedence over the current repository layout while the
migration is incomplete. Existing platform implementations under `shared/` or
Plugin crates are violations to remove, not patterns to copy.

## Architectural boundary

Barracuda uses Embassy as its execution framework on every supported target,
including host systems. Embassy is not a device-only implementation detail: it
is the common task, timer, and lifecycle model that keeps Platform behavior
consistent. A dedicated selected-target crate reads the Board and Platform
build selections and exposes their statically dispatched resource factory. The
fixed Embassy entry depends only on that crate, obtains `PlatformResources`,
and passes them to System to construct the fixed Plugin set.

```text
Embassy entry
    |
    v
independently selected Board YAML + Platform YAML
    |
    v
barracuda_target::resources(spawner)
    |-- generated SelectedPlatform + BOARD
    |-- SelectedPlatform::prepare()
    `-- SelectedPlatform::initialize(spawner, &BOARD)
        |-- validate the Board against its Platform-native layout
        |-- project native regions into Embassy storage capabilities
        `-- initialize platform resources and permanent driver tasks
    |
    v
System::new(lanes, resources, configuration)
    |-- construct Event Router and Plugin Manager
    `-- construct the fixed Plugin set
```

Do not add a generic `Platform::run` merely to wrap the Embassy executor.
Embassy already owns executor startup on host and device targets. The
Platform boundary is asynchronous initialization with an
`embassy_executor::Spawner`, invoked only by the selected-target crate. The
Embassy application entry must not parse selection YAML, import a concrete
Platform crate, or name the generated Board/Platform types itself.

A representative shape is:

```rust,ignore
pub trait Platform {
    type Network: 'static;
    type FileSystem: FileSystem;
    type DatabaseRegion: embedded_storage_async::nor_flash::NorFlash;
    type ModelApiFactory: 'static;
    type Error;

    fn initialize(
        spawner: embassy_executor::Spawner,
        board: &'static barracuda_board::Board,
    ) -> impl Future<
        Output = Result<
            PlatformResources<
                Self::Network,
                Self::FileSystem,
                Self::DatabaseRegion,
                Self::ModelApiFactory,
            >,
            Self::Error,
        >,
    >;
}
```

This shape is architectural guidance rather than a frozen API signature. Keep
the ownership and dependency boundaries below when refining the Rust API.
`PlatformResources` is constructed by the selected-target facade. Callers do
not assemble its fields or choose implementations through dependency injection.

A representative composition entry is:

```rust,ignore
let resources = barracuda_target::resources(spawner).await?;
let system = System::new(lanes, resources, workflow_directory).await?;
```

## Board versus Platform

A Board is one concrete hardware product. Its bundle owns fixed facts: chip
identity, pin assignments, attached peripherals, clock choices, native boot
layout files, and product-specific feature wiring. The platform-neutral
`Board` value generated from `board.yml` contains only common hardware facts
and logical capability bindings. It does not contain a second physical
partition table.

A Platform is a reusable implementation family such as ESP32, Host, STM32, or
test. It owns drivers, Embassy tasks, and adapters that turn one Board's fixed
facts into portable Barracuda capabilities. ESP32 is therefore one Platform,
not one Board. An ESP32 Platform implementation should be generic over the
supported ESP32 Board description rather than copied per product.

Board and Platform selection is build configuration, not a Rust type
relationship. Board YAML files live only under `boards/configs/`; Platform
YAML lives with its implementation under `platforms/<name>/platform.yml`. The
build independently selects one of each, validates them, and generates a
static [`barracuda_board::Board`] value plus the concrete Platform type. A
Board must not select a Platform through an associated type, Cargo dependency,
or Rust module reference.

The common API is the maximum useful intersection across Platforms. Physical
layout is deliberately not part of that intersection: ESP32 uses its ESP-IDF
partition table, STM32/nRF/RP use linker and bootloader layout symbols, and
Host uses a native image-layout document. Vendor-only flags remain native.

The native boot format is the sole source of physical truth. Platform build
code may validate it and project named regions into Embassy capabilities; it
must not generate it from a Barracuda-wide physical table. Consequently OTA
slots such as ESP `ota_0`/`ota_1`, Embassy Boot active/DFU/state regions, and
vendor metadata remain visible to the boot ecosystem that owns them. There is
no universal Barracuda partition format and no cross-vendor layout converter.

`board.yml` binds portable roles such as `database`, `filesystem`, and
`web-assets` to names in that native layout. Board and Platform remain
independent selections: declaring `hardware.chip: esp32c6` is a compatibility
fact, not a request to select the ESP32 Platform. The independently selected
Platform must reject an incompatible chip or missing native layout.

## Dependency direction

```text
shared contracts <------ Plugins
       ^
       |
Board API + Platforms
       ^
       |
build configuration selects Board YAML and Platform YAML
```

- `shared/` defines portable contracts and platform-neutral algorithms only.
- `platforms/` owns every concrete Network, FileSystem, and Flash
  implementation, including native and test implementations.
- Plugins consume only contracts. They never implement support for a concrete
  platform type.
- Plugin implementations and `PluginContext` remain platform-agnostic. They
  depend on semantic capabilities such as `PluginStorage`, never `NorFlash`,
  partitions, or concrete Platform types. The System/Plugin Manager composition
  seam may own the concrete database region needed to construct that storage.
- `platforms/selected` is the Platform-resource composition root. Build
  configuration independently selects one Board YAML and one concrete Platform
  YAML there.
- System is the runtime/Plugin-graph composition root and consumes the selected
  resource bundle without knowing how YAML selection was performed.

Adding a Platform must be possible by adding `platforms/<name>` without
modifying any Plugin.

## Repository layout

```text
core/
|-- system/
`-- plugin-manager/

boards/
|-- api/               # no_std static Board description
|-- config/            # host-only YAML parser, validator, and code generator
`-- configs/
    `-- <board>/
        |-- board.yml  # common facts and logical capability bindings
        `-- ...        # native files: partitions.csv, memory.x, host-layout.yml

platforms/
|-- api/               # barracuda-platform: trait and PlatformResources
|-- selected/          # generated Board + Platform selection and resource factory
|-- host/              # real host implementation
|-- test/              # deterministic test implementation
`-- <device>/          # one concrete embedded platform

shared/
|-- net/               # network contracts only
`-- fs/                # filesystem contracts only
```

Concrete implementations are forbidden in `shared/`, including test doubles.
For example:

- Host network and disk filesystem implementations belong to
  `platforms/host`.
- scripted/never network, memory filesystem, and memory NOR flash belong to
  `platforms/test`.
- Embassy network adapters, hardware filesystems, flash partitions, and runner
  tasks belong to the relevant device Platform.

## Native storage realization

The portable storage boundary is an Embassy/embedded-storage capability, not
a table format:

| Platform family | Physical source of truth | Runtime realization |
| --- | --- | --- |
| ESP32 | Board `partitions.csv`/binary consumed by ESP-IDF tooling | `esp-storage::FlashStorage` + Embassy async adapter + `Partition` |
| STM32 | Board `memory.x` and bootloader symbols | `embassy_stm32::flash::Flash` + Embassy `Partition` |
| nRF/RP | Board linker/Embassy Boot symbols | the corresponding Embassy HAL flash + `Partition` |
| Host | Board `host-layout.yml` image description | file-backed NOR + Embassy `Partition` |

Filesystem choice is above this layer. A FAT image, LittleFS image, or another
format is provisioned into a native region; the Platform exposes its block or
file capability without making Picoserve understand partition tables. Ekv is
given only the writable database NOR region. Read-only Web assets are never
issued as a writable NOR capability.

`shared/` may expose generic contract-conformance helpers, but those helpers
must accept an implementation from a Platform crate and must not instantiate a
backend themselves.

## Platform-owned tasks

The concrete Platform owns every task required to keep a platform service
alive. A Network handle alone is not a running network stack. For Embassy Net,
the Platform initializes both `Stack` and `Runner`, stores the handle in static
platform state, and spawns a concrete task that owns the Runner.

```rust,ignore
#[embassy_executor::task]
async fn network_task(mut runner: ConcreteNetworkRunner) {
    runner.run().await
}
```

The same rule applies to a filesystem or device backend that requires a
permanent driver loop. System and Plugins receive handles; they never receive,
poll, or spawn concrete platform runners.

Platform tasks are statically allocated Embassy tasks. Do not replace them
with boxed futures, runtime registries, or a `PlatformRuntime` future stored in
System.

## Embassy on Host Platforms

Host is a Platform implementation, not an alternate execution architecture.
Its application entry, System future, Plugin Components, timers, and permanent
service tasks follow the same Embassy lifecycle as device Platforms.

A Host Platform may use operating-system APIs or a host async reactor behind
its concrete Network, FileSystem, and Flash adapters. That backend mechanism
must remain encapsulated by `platforms/host`: it must not make System or a
Plugin select Tokio, async-std, or any other executor, and it must not require a
different Plugin lifecycle. In particular, a Tokio `main` that directly polls
System is a migration-state violation, not the Host Platform design.

The desired invariant is:

```text
                   common Embassy executor/task model
                         /                 \
              Host Platform             device Platform
        OS/reactor-backed adapters      HAL-backed adapters
                         \                 /
                    identical System + Plugins
```

Prefer portable Embassy facilities such as `embassy-time` everywhere. A
Platform-specific timer or task abstraction is justified only when Embassy
cannot express the required hardware behavior.

## Network ownership

The concrete Platform defines a local, zero-cost Network adapter type around
its real network stack. It implements the portable contracts required by the
system, such as DNS, outgoing TCP, UDP, and incoming TCP listen/accept.

```rust,ignore
#[repr(transparent)]
pub struct DeviceNetwork {
    stack: embassy_net::Stack<'static>,
}
```

The Platform returns a shared static handle:

```rust,ignore
network: &'static DeviceNetwork
```

This matches long-lived network clients and the static storage required by
Embassy Net. Time, WebServer, and Agent share the same handle and create their
own sockets. Socket capacity is concrete Platform configuration.

Network runner ownership is separate:

```text
Platform task owns Runner
System and Plugins share Network handle
```

`shared/net` must not contain Tokio or Embassy implementations. It defines the
contracts. Each Platform crate implements those contracts for its own local
adapter type.

## Filesystem ownership and contract

`FileSystem` is a lightweight handle contract, not a backend object that every
consumer wraps in `Arc`.

The target contract must require a cheap clone that preserves filesystem
identity:

```rust,ignore
pub trait FileSystem: Clone + 'static {
    type File: FsFile;
    // asynchronous, statically dispatched operations
}
```

Cloning a FileSystem handle must be O(1), must not copy stored data, and must
address the same namespace. The concrete implementation chooses its internal
sharing mechanism. Framework code must not impose `Arc<F>`, `Rc<F>`, a leaked
`&'static F`, or dynamic dispatch around a generic filesystem handle.

Platform returns one handle by value. System clones it for Event Router and
filesystem-consuming Plugins. Event Router owns its handle. Agent Runtime owns
its handle and clones it for its internal persistence consumers.

Filesystem I/O must be asynchronous and statically dispatched so flash, SPI,
SD, or another cooperative backend cannot block Time, Scheduler, Network, and
other Embassy tasks. Do not use `async-trait` or boxed trait futures for this
contract.

The persistence contract must expose the recovery semantics Barracuda actually
needs:

- reliable append that cannot corrupt previously committed records;
- atomic/durable replacement implemented by the backend, not a default
  `create + write + rename` approximation;
- fallible existence checks (`Result<bool, FsError>`);
- rooted implementations that reject parent traversal and cannot escape their
  configured root;
- consistent directory semantics across native, device, and test Platforms.

Low-level primitives must not let callers bypass the documented append and
replacement disciplines.

## Flash ownership and partition composition

The Board bundle carries the vendor-native physical layout, while `board.yml`
binds logical roles into it. The selected Platform owns the one physical flash
driver, validates the complete native table, creates
non-overlapping regions, and mounts concrete filesystems. This must happen
inside the selected-target factory, before System consumes `PlatformResources`.

Portable partition handles use the standard asynchronous NOR contract:

```rust,ignore
type DatabaseRegion: embedded_storage_async::nor_flash::NorFlash;
```

Do not expose `ekv::flash::Flash` from Platform. That trait is an ekv-internal
database interface and would couple Platform to the current persistence engine.
Platform returns the already mounted filesystem handle and only the database
NOR region needed by System; it never returns a second, independently owned
view of the same entire physical flash.

```text
Board partition table
        |
        v
Platform-owned physical Flash
|-- filesystem region
|   `-- Platform mounts concrete filesystem and returns its handle
|-- database region
|   `-- System -> Plugin Manager -> ekv / scoped Plugin storage
`-- OTA or future regions
```

Plugin Manager receives only its database region, never the entire physical
flash. Inside Plugin Manager, a private local adapter implements
`ekv::flash::Flash` over the region's async `NorFlash` implementation.

The physical type stops there. Plugin implementations receive the semantic
`PluginStorage` contract through `PluginContext`; neither `Plugin` nor its
Components name the database region or any NOR trait.

The adapter must validate at least:

- ekv page size versus physical erase size;
- ekv alignment versus flash read/write alignment;
- region capacity versus ekv page size;
- erased-value compatibility.

Plugin Manager constructs and mounts ekv itself, then derives one scoped KV
store per Plugin identity. Platform never constructs or exposes Plugin storage.

## Plugin rules

Plugins are self-contained system-managed units. They declare portable
capability requirements and obtain initialized system handles through the
system/Plugin context boundary. They must not receive concrete platform
objects through application-level dependency injection.

Guidance for choosing between Event Router contracts and typed Plugin
capabilities lives in
[`plugin-communication.md`](plugin-communication.md).

Forbidden in Plugin crates:

- dependencies on a concrete Platform crate;
- `impl` blocks for Tokio, Embassy Net, or chip-specific types;
- platform-selection Cargo features such as `tokio`, `embassy`, or a chip name;
- spawning or polling a platform runner;
- constructing a filesystem, network stack, or raw flash;
- exposing `ekv::flash::Flash` as a platform contract.

WebServer is the current example to correct. Its Plugin must consume a portable
TCP listen/accept contract from `shared/net`. Embassy and Tokio listener
implementations belong to their Platform crates. Picoserve request routing and
generic connection adaptation remain inside WebServer because they are Web
server behavior, not platform initialization.

## Test policy

Tests are not exceptions to the Platform boundary.

- Deterministic tests use `platforms/test`.
- Real host integration tests use `platforms/host`.
- Core and Plugin test targets add the appropriate Platform crate as a dev
  dependency.
- No `MemFs`, `NeverStack`, `ScriptedStack`, `MemFlash`, or comparable concrete
  backend remains in `shared/`.
- Each Platform runs the common contract-conformance suite against its own
  implementation.

The test Platform may expose direct control and observation handles for
scripts, recorded traffic, failure injection, filesystem contents, and flash
fault simulation. Those APIs are test-only Platform APIs, not shared contracts.

## Zero-overhead requirements

The Platform abstraction must preserve these properties:

- concrete Platform selected at compile time;
- Embassy executor, task, and time semantics on host and device Platforms;
- no `dyn Platform`, `Any` Platform registry, or runtime Platform lookup;
- no boxed Platform task futures;
- platform tasks statically allocated by Embassy;
- Network and FileSystem calls statically dispatched and eligible for inlining;
- no framework-mandated reference counting around FileSystem handles;
- no Platform generic parameter propagated into Plugin Manager or Plugin
  Context;
- any typed capability lookup occurs only during Plugin registration/start and
  never on network, filesystem, RPC, event, or scheduler hot paths.

Monomorphization for the one selected Platform is intentional. Generic
contagion through unrelated core infrastructure is not.

## Review checklist

Before adding or changing a platform-sensitive feature, verify:

1. Is the file under `platforms/` if it instantiates a real or test backend?
2. Does `shared/` contain only contracts and platform-neutral algorithms?
3. Can a new Platform be added without editing a Plugin?
4. Does the Platform own and spawn every required permanent driver task?
5. Are Network and FileSystem exposed as handles rather than their runners?
6. Does the Board own fixed layout while the reusable Platform validates and
   realizes it before returning resources?
7. Are Board and Platform selected independently by build configuration, with
   no Board-associated Platform type?
8. Does Plugin Manager remain unaware of the selected Platform type?
9. Are calls statically dispatched outside one-time Plugin capability lookup?
10. Does a test use `platforms/test` or `platforms/host` instead of a shared
   concrete backend?
11. Does Host preserve the Embassy execution model instead of making System
    or Plugins depend on a host executor?
12. Does the application obtain resources from `barracuda-target` instead of
    parsing selection YAML or importing a concrete Platform itself?
