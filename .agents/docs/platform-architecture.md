# Platform Architecture

This document defines Barracuda's target architecture. It is authoritative for
the meaning and ownership of Platform, Board, Peripheral, chip Driver, HAL, Target, System, and
Plugin. Current code that contradicts these boundaries is migration work, not
precedent.

## Core model

Barracuda has two independently selected axes and one Platform-owned hardware
boundary:

~~~text
Platform  = platform services + vendor adaptation to ecosystem HAL contracts
Board HAL = Board matrix + peripheral implementations + exposed I/O
Target    = Platform + Board HAL
~~~

- **Platform** is one execution platform: a chip line such as ESP32-S3,
  ESP32-C6, or STM32, or a hosted environment such as Linux or macOS. It provides platform mechanisms such as
  an IP stack, entropy, and partitions and adapts its vendor HAL into upstream
  hardware contracts. It does not describe a Board's concrete peripherals,
  wiring, or product hardware matrix.
- **Board** is one concrete hardware combination. Its matrix describes the
  chip, buses, pins, clocks, attached peripherals, fixed wiring, and native
  physical layout of that product.
- **Peripheral API** is a semantic, chip-independent interface such as
  Display, Touch, PowerMonitor, or RealTimeClock.
- **Peripheral implementation** adapts concrete chips and wiring to one
  Peripheral API. It may compose multiple private chip Drivers.
- **Chip Driver** is a low-level register or protocol implementation under
  `drivers/chips`. It is neither Board-visible nor System-visible and owns no
  peripheral manifest.
- **Platform HAL module** is vendor adaptation implemented beside each
  Platform. It converts the selected Platform's move-only tokens into concrete
  values implementing `embedded-hal`, `embedded-hal-async`, or a narrow
  domain binding where those ecosystems have no contract.
- **Board HAL** is the statically composed result of applying peripheral
  implementations to a Board matrix through the selected Platform HAL module.
  It exposes optional semantic peripherals and explicitly exposed I/O for
  System and hardware Plugins to consume.
- **Target** is the independently selected Platform combined with the selected
  Board HAL.
- **System** is the aggregation entry. It constructs system-owned services,
  assembles the fixed Plugin graph, and routes Platform and HAL capabilities to
  their consumers.
- **Plugin** is a self-contained functional module managed by System.

A Platform is not a Board support package, a peripheral-driver collection, or a
System dependency bag. Its HAL module contains reusable vendor adaptation, not
product wiring.

`TargetIdentity` is immutable build-time metadata describing the independently
selected Platform and Board. It is composed alongside, but never inside,
`TargetResources`: it grants no hardware authority and does not change the
exact `BoardResources { peripherals, exposed_io }` shape. System may copy this
metadata into observational adapters such as `vm-systeminfo`.

## Composition boundary

Board and Platform selection are independent build inputs. A Board never
selects its Platform through a Rust associated type, Cargo dependency, or YAML
reference.

~~~text
one hardware bootstrap --------+------------------------------+
                               |                              |
selected Platform -------------+-> Platform bindings -> Platform services
             `--------------------> Platform HAL
                                                              |
selected Board config -> generated Board composition <--------+
  +-- peripheral implementation + private I/O -> peripherals  |
  `-- explicitly exposed I/O -> GPIO/I2C/SPI providers        |
                               |                              |
                               +----------> Board HAL ---------+
                                                              |
                                                              v
                                                            Target
                                                              |
                                                              v
                                                   barracuda_target::resources
                                                              |
                                                              v
                                                            System
                                                              |
                                                              v
                                                           Plugins
~~~

The selected-target composition root validates that the independently selected
Platform and Board can form one Target. The selected Platform exposes a
compile-time entry macro that owns only the target ABI, executor bootstrap, and
process or firmware termination behavior. The System application owns its
application callback, receives the selected Target's typed `TargetBindings`,
constructs Target resources, and runs System. The macro expansion calls that
callback directly: it introduces no dynamic dispatch, boxed future, or runtime
registry.

A device Platform acquires the chip HAL's singleton exactly once when producing
those bindings. The common Target composition then splits the bindings, invokes
the Platform and Board constructors, and returns their resources to System
without flattening one axis into the other. The composition follows the selected
Board declarations; it does not infer an I/O surface from hardware that the
Board config omitted. Platform entries do not parse YAML, instantiate
peripheral implementations, wire individual Board peripherals, or depend on System.

~~~rust,ignore
async fn application(spawner: Spawner, bindings: barracuda_target::Bindings) -> Result<(), Error> {
    let resources = barracuda_target::resources_with_bindings(spawner, bindings).await?;
    barracuda_system_app::run(spawner, resources).await?;
    Ok(())
}

barracuda_target::application_entry!(application);
~~~

Host Targets have no chip peripheral singleton. The Linux and macOS Platforms
share a host-only virtual one instead (`platforms/virtual-io`): virtual pins
and I2C controllers backed by a virtual peripherals manager that tests and
developers drive over a loopback control interface. The host entry macro takes
that singleton once, exactly as a device entry takes its chip singleton, and
the generated Board bindings move the declared virtual tokens out of it. The
application still uses the same `resources_with_bindings` boundary, and the
virtual hardware never enters firmware.

Host Boards declare peripherals in `board.yml` like device Boards. Because
the host has no physical chips, its Platform HAL manifest sets
`peripheral-models`: generated Board composition then announces every
declared peripheral (name, implementation identifier, chip-native bindings,
and scalar parameters) to `hal::declare_peripheral` before initializing any
of them, and the virtual Platform attaches the chip model for that
implementation at the declared bus and address. The Board stays the single
declaration and the generator keeps no Platform or implementation special
case. Device Platforms do not set it, so their generated composition is
unchanged.

The returned shape preserves ownership:

~~~rust,ignore
TargetResources {
    platform: PlatformResources { ip_stack, wifi, entropy, partitions },
    board_hal: BoardResources { peripherals, exposed_io },
}
~~~

Board peripherals never become fields of `PlatformResources`. Platform services
never become fields of a Board resource bundle.

## Platform

A Platform represents one execution platform: an ESP chip such as ESP32-S3
or ESP32-C6, an STM32 line, Linux, macOS, or an equivalent environment. Each
ESP chip is its own Platform crate and selection (`platforms/esp32`,
`platforms/esp32c3`, `platforms/esp32s3`, ...); chips that share a vendor
ecosystem declare the same `family` in `platform.yml` and may share vendor
adaptation source, but they are never merged into one family Platform. A
Platform owns integration with its execution environment and supplies
platform-level services. Concrete Board peripherals do not become Platform
fields or Platform associated types.

Platform resources have stable, exact shapes. The current common contract
exposes one Embassy IP stack, one Wi-Fi control mechanism, one entropy source,
and one partitions collection:

~~~rust,ignore
pub struct PlatformResources<Partitions, Wifi, Entropy> {
    pub ip_stack: embassy_net::Stack<'static>,
    pub wifi: Wifi,
    pub entropy: Entropy,
    pub partitions: Partitions,
}
~~~

`wifi` is a separate communication mechanism, not part of `ip_stack`.
Platforms without a radio supply an explicitly unsupported implementation, and
System hands the mechanism to the Wi-Fi Plugin, which owns the policy.

`entropy` is the Platform's source of unpredictable bytes, fit for
cryptographic use: the operating system's generator on Host Platforms, a true
random number generator fed by physical noise on devices. A Platform without
one supplies `UnavailableEntropy`, never a weaker generator. System builds the
TLS engine from it and hands it to the VM Plugin, which seeds `math.random`
from it; like Wi-Fi, the Plugin's constructor is generic over the Platform's
type and erases it inside.

This is architectural guidance rather than a frozen Rust signature. The
invariant is that partitions remain a collection. Business roles never become
fields such as filesystem_partition, resources_partition, or
database_partition. The IP capability likewise remains `ip_stack` rather than
web_network or database_network. TLS is not a Platform resource; System builds
it above the Platform boundary (see "TLS and HTTP clients").

Adding a Plugin, peripheral, filesystem, database, or application subsystem
does not modify the Platform API or PlatformResources shape.

Platform initialization may start tasks required by its own services. It does
not start tasks belonging to a peripheral implementation. Implementation
lifecycle belongs to the HAL composition that owns it.

## Board

A Board describes one concrete product hardware matrix:

- exact chip and package;
- buses, pins, DMA channels, interrupts, and clocks;
- attached peripheral models and their fixed wiring;
- explicitly exposed multifunction pins;
- external memories and storage devices;
- native boot and physical layout artifacts;
- product-specific hardware feature presence.

Board YAML and Board-related crates live under boards/. A Board matrix is data
used to construct a HAL; it does not implement or select a Platform.

Board configuration identifies concrete hardware and the native names required
to construct it. It does not contain mounted filesystems, database objects,
Plugin instances, or application services.

Board selection is an explicit persistent step:

~~~bash
cargo board select
cargo build
~~~

The selection command presents a colored fuzzy-searchable list, validates the
chosen Board bundle, and records its name in workspace-local ignored state. It
also accepts an explicit Board name for automation. Each Platform declares the
rustup toolchain used to compile it, any standard-library crates Cargo must
build for its target, an optional installer-generated environment file, and
the prompt shown when that toolchain is unavailable. Selection verifies that
declaration, applies the workspace rustup override, and writes the Cargo
configuration needed by ordinary `cargo check`, `cargo build`, and `cargo run`
commands. Host-side Cargo tools use a complete host standard library and an
isolated artifact directory while an embedded `build-std` policy is active.
The selector crates keep stable source manifests. `cargo board` bootstraps
ignored local dependency packages under `.barracuda/selection` before Cargo
resolves the source workspace, then activates only the concrete Platform and
peripheral implementation dependencies required by the chosen Board. Every
local package declares the complete optional dependency catalog, so changing
the selected default features does not rewrite `Cargo.lock`. The build reads
persistent workspace state; it does not require shell setup or a Board-aware
build wrapper.
`boards/selected` must not inspect `target_os` or `target_arch`, and the outer
Target composition must not infer a default Board or activate a Board feature
from them. A missing selection is a build error that points back to the select
command. This keeps target policy out of Board parsing and code generation
while preserving Board and Platform as independent inputs.

## Board-declared hardware surface

The selected Board config is the source of truth for the hardware surface that
Barracuda constructs. That surface has exactly two fields:

- `peripherals` names optional attached devices, their implementation, and the
  private pins, buses, geometry, polarity, and other fixed inputs used to
  construct them;
- exposed I/O names physical resources that applications may configure at
  runtime, beginning with multifunction package pins.

The generated Board composition constructs exactly those declarations. It does
not expose unused pins or peripheral instances automatically. A physical
resource omitted from `exposed-io` is absent from the Barracuda I/O surface even
when the chip could otherwise use it.

Peripheral composition consumes its private resources before runtime I/O is
constructed. One chip-native token cannot be both a peripheral binding and an
exposed resource.
Within exposed I/O, a physical pin is declared once even when the selected
Platform can route digital, analog, PWM, I2C, SPI, UART, or I2S functions to it.
The function is selected when an application opens a handle; it is not encoded
as a second logical copy of the pin in Board YAML.

Generic validation covers the Board schema, referenced implementation identifiers,
required wiring fields, and whether the selected Platform declares the required
HAL bindings. Move-only generated bindings prevent one chip token from being
consumed twice. Platform providers validate runtime routing, electrical modes,
frequencies, and controller limits before constructing a function handle.

## Peripherals and exposed I/O

Board configuration distinguishes optional attached product hardware from
physical resources that Barracuda intentionally makes available to
applications:

~~~yaml
peripherals:
  io: {}
  devices:
    status-indicator:
      implementation: indicator-led
      bindings:
        pin: gpio10
      parameters:
        active-level: low

exposed-io:
  pins:
    expansion-1: { pin: gpio2 }
    expansion-2: { pin: gpio3 }
    expansion-3: { pin: gpio4 }
    expansion-4: { pin: gpio5 }
~~~

The concrete schema stores hardware facts, implementation identifiers, stable
application-visible names, and chip-native identifiers. Rust crate paths,
Platform types, Plugin identities, and application policy do not belong in the
Board matrix. An implementation identifier selects registered composition
code; YAML does not name a Rust type or crate path.

Board resources contain no third hardware category:

~~~rust,ignore
BoardResources {
    peripherals: GeneratedPeripherals { /* Option<display>, Option<rtc>, ... */ },
    exposed_io: GeneratedExposedIo { /* move-only tokens, Platform providers */ },
}
~~~

`BoardResources<Peripherals, ExposedIo>` is the common ownership envelope. Its
generic fields preserve the concrete, statically composed peripheral types for
each selected Board. Every generated peripheral field is `Option<T>` because no
peripheral kind is mandatory. The single exposed-I/O value owns all runtime
physical resources.
System places it in `PluginContext` behind one shared `Arc`; hardware VM Plugins
clone that owner, not independent protocol collections. Platform-owned
controller pools are part of this owner but are not application-visible names.

Controller, channel, timer, DMA, and buffer inventory belongs to the Platform
manifest rather than `exposed-io`. Resource families that must be acquired
together are declared as typed controller groups:

~~~yaml
hal:
  runtime-uart-controllers: [UART1]
  runtime-adc-controllers:
    - controller: ADC1
      channels:
        - { channel: CHANNEL0, pin: GPIO1 }
  runtime-pwm-controllers:
    - controller: LEDC
      timers: [TIMER0]
      channels: [CHANNEL0]
  runtime-i2s-controllers:
    - controller: I2S0
      dma-channels: [DMA_CH0]
      dma-buffer-bytes: 4096
~~~

The Board generator removes an entire runtime group when a peripheral binding
already consumes its controller or DMA channel. It then constructs fixed-size,
concrete pools; firmware never scans the chip or infers missing resources.
ADC channel routes retain their controller and channel identity after the
physical pin enters the shared owner. PWM handles consume both a timer and an
output channel. I2S handles consume a controller, DMA, and bounded static
buffers together with their pins.

`ExposedIo` marks this shared ownership boundary. Focused provider traits open
functions on it and return concrete, statically dispatched values implementing
upstream contracts:

| Hardware use | Upstream contract |
| --- | --- |
| dynamically configured digital GPIO | Barracuda `ConfigurableDigitalPin`, with operations from `embedded_hal::digital` |
| asynchronous GPIO edge wait | `embedded_hal_async::digital::Wait` |
| analog input or output | Barracuda `AnalogInput` or `AnalogOutput` |
| PWM output | `embedded_hal::pwm::SetDutyCycle` |
| I2C controller handle | `embedded_hal_async::i2c::I2c` |
| exclusively owned SPI controller handle | `embedded_hal_async::spi::SpiBus` |
| one device on a shared SPI controller | `embedded_hal_async::spi::SpiDevice` |

Barracuda does not replace operations that upstream HAL traits already define.
`ConfigurableDigitalPin` adds the missing runtime transition between disabled,
input, and output states. `AnalogInput` and `AnalogOutput` cover ADC and DAC
because `embedded-hal` 1.0 has no corresponding contracts. Stable Board names,
static storage, pin mux, controller construction, and hardware teardown come
from generated Board composition and selected Platform HAL providers.

The runtime owner contains one protocol-neutral catalog of move-only Platform
tokens. Providers for digital, analog, PWM, I2C, SPI, UART, I2S, and future
function families all claim through this same owner. A digital handle consumes
one pin. An I2C handle atomically consumes a controller, SCL, and SDA. An I2S
handle may consume a controller, clock and data pins, and DMA. Duplicate roles,
unavailable routes, or any claimed resource fail without partially changing
ownership.

Applications name exposed pins and function parameters, not chip controller
instances. For example, `i2c.open(scl, sda, frequency)` asks the provider to
select a compatible free controller. This keeps controller allocation and
peripheral reservations in Platform composition and gives the same application
API to Boards with different controller numbering.

Reusable peripheral implementations import `embedded-hal`, `embedded-hal-async`, and established
domain ecosystem traits directly. Barracuda defines a narrow binding only when
the ecosystem has no suitable contract. Current examples are parallel camera
frame reception and PCM streaming; they live with the peripheral API
rather than forming a second general-purpose HAL. The selected build keeps
concrete storage and dispatch statically allocated; consumers do not look up a
Platform, Board, or peripheral implementation at runtime, and async HAL traits are
not converted into `dyn` trait objects.

`embedded-hal` does not standardize changing a GPIO between input and output
modes. Every VM-exposed digital handle therefore implements the small Board HAL
configuration contract in addition to the upstream operation traits. Protocol
alternate functions are established by the corresponding provider, not by the
digital API. Each Platform's `hal` module owns vendor pin mux and controller
construction.

Peripheral buses use existing upstream sharing adapters. HAL-owned buses use the
blocking or async `embassy-embedded-hal` or `embedded-hal-bus` adapter whose
ownership model fits the concrete implementation. Implementation code receives `SpiDevice` when
it addresses a CS-selected device and `SpiBus` only when it truly owns the
whole bus. Display implementations such as `mipidsi` continue to receive these
upstream values and remain outside dynamic exposed-I/O ownership.

The GPIO, I2C, and SPI Plugins are adapters over the shared runtime owner. They
validate script-level arguments and ask their provider to atomically open a
complete function. The returned HAL value lives in Lua userdata and operations
call its upstream traits directly. Explicit `close`, lexical `<close>`, garbage
collection, and VM teardown disable and drop the constructed function. They do
not recreate vendor singleton tokens: the generated device owner keeps the raw
pin and controller claimed until restart. Plugin revocation immediately
invalidates operations. A GPIO claim therefore prevents the same pin from
being opened as I2C or SPI during that boot, even though those functions belong
to different VM packages.

Board HAL has no scripting service contract. VM packages do not perform pin
mux, reconstruct vendor peripherals, or keep protocol-local ownership maps.
Runtime dispatch is limited to Board-name lookup and provider selection; data
operations, Board composition, and peripheral implementations remain
statically dispatched. The detailed ownership and application-pressure model
is documented in `boards/hal/docs/design.md`.

## Peripheral implementations, chip drivers, and HAL

Peripheral implementations adapt reusable chip behavior to semantic APIs. The
Board matrix supplies the concrete bus and wiring values used to instantiate them:

~~~text
Platform HAL + Board matrix + peripheral implementations -> Board peripherals
~~~

For a display:

~~~text
Board matrix
+-- display controller model
+-- SPI or parallel bus
+-- chip-select, data/command, reset, and backlight pins
+-- DMA and interrupt bindings
+-- geometry and orientation
             |
             v
Display peripheral implementation
             |
             v
Display peripheral
             |
             +----> System
             +----> Display-consuming Plugin
~~~

The consumer receives the semantic Display peripheral. It does not reconstruct
the implementation from raw pins or import the concrete Board crate.

Adding a Display changes the Board matrix, peripheral composition, and
the System or Plugin wiring that consumes Display. It does not change Platform
or PlatformResources.

Implementation tasks and interrupt-facing state are owned by the HAL composition
that instantiated the implementation. System and Plugins receive handles or
peripherals; they do not own the implementation runner.

Board HAL initialization consumes generated owned bindings rather than finding
hardware by number or acquiring the chip singleton itself. Its conceptual
contract is:

~~~rust,ignore
trait BoardHal {
    type Bindings;
    type Resources;
    type Error;

    async fn initialize(
        spawner: Spawner,
        bindings: Self::Bindings,
    ) -> Result<Self::Resources, Self::Error>;
}
~~~

The runtime `Board` descriptor remains useful for identity, diagnostics, and
native-layout selection. It is not a second source of peripheral ownership.
The device entry acquires its chip singleton and constructs one
`TargetBindings<PlatformBindings, BoardHalBindings>` value. Selected Target
composition consumes and splits that value before invoking the two
initializers. Rust types enforce which binding values can reach each axis;
chip compatibility validation remains in the concrete binding constructors.
Neither axis names or selects the other.

## Partitions

Partitions are a Platform service presented as one generic collection. A
Platform implements the mechanism appropriate to its family, while the
selected Board supplies the concrete native layout artifact for the product.

| Platform | Native source |
| --- | --- |
| ESP | ESP partition CSV or binary |
| STM32 | linker and bootloader layout symbols |
| nRF/RP | linker and Embassy Boot symbols |
| Linux/macOS | file-backed image layout |

The native format remains authoritative. Barracuda does not introduce a second
cross-platform physical partition-table format or translate one vendor's table
into another vendor's table.

Platform initialization validates the selected native layout and exposes its
usable regions through partitions. Each entry carries native access and
on-media format metadata (`raw`, `fatfs`, or `littlefs`) copied from that
partition-table entry. The collection supports arbitrary entries; its Rust
type does not grow a field for every consumer.

~~~text
PlatformResources
+-- partitions
    +-- partition A
    +-- partition B
    +-- partition C
    +-- ...
~~~

System assigns business meaning after taking regions from the collection:

~~~text
partitions
+-- system region            -> mount mutable LittleFS
+-- resources region         -> mount selected read-only FATFS or LittleFS
+-- region selected by System -> open ekv
+-- remaining regions         -> OTA, boot state, or future consumers
~~~

Platform knows the declarative on-media format because it projects the native
partition table. It does not construct LittleFS, FATFS, ekv, Event Router,
WebServer, Plugin Manager, or Plugin identities. System owns those
constructions.

A Plugin normally receives scoped semantic storage from System. If a product
requires a new dedicated physical region, its native Board layout gains that
region and System consumes it from partitions; the Platform API remains
unchanged.

## Filesystems and database

System constructs application storage from Platform partitions, memory, and
filesystem-producing peripherals supplied by the selected Target. Filesystem
and database objects are System-owned software services rather than Platform
resources.

`shared/vfs` is the filesystem mechanism layer. It defines the backend
contract, independent mount namespaces, path resolution, portable file
operations, and mount-management-free `ScopedVfs` views. It has no knowledge of
Barracuda's semantic roots, Plugin layout, Workspace, partitions, or concrete
filesystem selection. System owns that application policy and composes
concrete backends into one process-wide VFS.

System selects the resources backend only from the partition metadata supplied
by Platform. It never reads image bytes or boot-sector magic to guess whether
the partition contains FATFS or LittleFS.

~~~text
Target partitions, memory, and removable filesystem peripherals
          |
          v
System application composition
  +-- FATFS
  +-- LittleFS
  +-- MemFS
  `-- ekv
          |
          +----> process-wide VFS ----> scoped Plugin views
          `----> Plugin Manager KV storage
~~~

### Semantic roots

System assigns every global VFS root a lifecycle contract and a default
backend:

| Semantic root | Default backend | Access and lifecycle |
| --- | --- | --- |
| `/resources` | FATFS or LittleFS selected by native partition metadata | Read-only image content. An upgrade may replace the complete filesystem. |
| `/data` | LittleFS | Durable read-write state required for correctness. System never evicts it. |
| `/cache` | MemFS | Read-write reproducible or temporary content. It starts empty after every restart and may be cleared while running. |
| `/media` | Target-selected installed filesystem; currently a System LittleFS subtree | Stable, durable, high-volume runtime content. Its physical medium may be soldered storage, installed storage, or FATFS on flash, but its namespace does not disappear while System is running. |
| `/removable` | Read-only MemFS namespace anchor plus live child mounts | Runtime namespace for filesystems that may appear and disappear. Each present medium is mounted read-write at `/removable/<slot-id>`; the anchor itself stores nothing. |

System mounts `/resources` read-only regardless of the filesystem selected by
the native Board layout. The default `/media` mount scopes the `/media` subtree
of the same persistent LittleFS backend mounted at `/data`; the separate VFS
mount preserves lifecycle boundaries and cross-mount rename behavior without
inventing another native partition. A future Target may supply a dedicated
installed bulk-media backend while preserving the same logical contract.

`Media` describes the stable large-storage role, not a bus or filesystem
format. `RemovableStorage` describes a Board-attached slot whose implementation
recognizes a filesystem and reports insertion and removal events. It does not
expose the SDMMC device, block operations, FAT driver, card-detect signal, or IO
expander to System. Those details remain inside the peripheral implementation
and chip drivers.

The generated Board peripheral instance name is the stable `slot-id`; for
example, a peripheral named `micro-sd` appears at `/removable/micro-sd`.
System owns the slot lifecycle task and dynamically mounts and detaches the
filesystem. Detach immediately removes the path from new lookups; handles that
outlive physical removal report `MediaRemoved` when their backend observes the
disconnected medium.

The VFS API exposes only each root's semantic contract. Filesystem selection,
formatting, mounting, and recovery remain in System composition. The ekv
database remains separate from the VFS and backs Plugin Manager's scoped
structured storage.

### Plugin and Workspace namespaces

The global VFS separates private Plugin trees from one shared Workspace:

~~~text
/resources/plugins/<plugin-id>/...
/data/plugins/<plugin-id>/...
/cache/plugins/<plugin-id>/...
/media/plugins/<plugin-id>/...

/resources/workspace/...
/cache/workspace/...
/media/workspace/...

/removable/<slot-id>/...
~~~

Plugin Manager derives one `ScopedVfs` for each filesystem-enabled Plugin:

| Plugin-visible path | Global source | Ownership |
| --- | --- | --- |
| `/resources` | `/resources/plugins/<plugin-id>` | Plugin-private |
| `/data` | `/data/plugins/<plugin-id>` | Plugin-private |
| `/cache` | `/cache/plugins/<plugin-id>` | Plugin-private |
| `/media` | `/media/plugins/<plugin-id>` | Plugin-private |
| `/workspace/resources` | `/resources/workspace` | Shared, read-only |
| `/workspace/cache` | `/cache/workspace` | Shared, disposable |
| `/workspace/media` | `/media/workspace` | Shared, durable |
| `/workspace/removable` | `/removable` | Shared, dynamically available |

Every filesystem-enabled Plugin sees the same Workspace content at the same
logical paths. Private paths remain isolated by Plugin identity. Shared state
required for correctness retains an explicit owner and belongs in that
Plugin's `/data` tree or scoped KV storage, so Workspace exposes only
`resources`, `cache`, `media`, and currently inserted removable filesystems.

`/workspace/resources` contains immutable common inputs supplied by the System
image. `/workspace/cache` is the short-lived exchange area for VM output and
other files passed among Plugins; System may clear it by run, by session, under
memory pressure, or during restart. `/workspace/media` contains shared user
uploads and generated files that must outlive the producing run; System does
not automatically evict it. `/workspace/removable/<slot-id>` exposes the root
of a currently inserted filesystem. It requires no capability beyond the
Plugin's ordinary filesystem declaration and disappears when that medium is
removed.

### Composition and operation invariants

The image builder places `filesystem/resources` contributions below
`/resources/plugins/<plugin-id>` and merges
`filesystem/workspace/resources` contributions below
`/resources/workspace`. A shared-path collision fails the image build.

System installs the stable semantic roots before Plugin Manager derives any
`ScopedVfs`. All VFS clones and scoped views share one live mount table, so a
later removable-filesystem mount is visible to already-running Plugins.
A scoped view supports file operations and path translation; mount, unmount,
backend inspection, concrete filesystem selection, and removable-media
lifecycle stay at the System boundary.

Every logical mount point is a directory of the view: it lists as empty until
its source directory exists, and it cannot be written, removed, or renamed, so
no Plugin can replace a private root or a shared Workspace directory with a
file. Parents of mount points, such as `/` and `/workspace`, list the mount
points below them. A rename never moves a directory into its own descendant.

Path normalization contains operations within exposed logical mounts. Rename
is confined to one mounted filesystem, so moving from `/workspace/cache` to
`/workspace/media` requires copy followed by removal. Atomic publication writes
a temporary file and renames it within the destination mount.

Plugin unload releases its handles and scoped view while preserving durable
`/data`, `/media`, and `/workspace/media` content. Cache content remains
disposable independently of Plugin lifetime.

## IP and communication capabilities

`network` is too broad to be a useful capability name. The current Platform
contract supplies an `embassy_net::Stack` named `ip_stack`, which means exactly
IPv4/IPv6, TCP, UDP, and the protocols enabled on that stack. System and
Plugins share this exact capability. Consumer-specific fields such as
`web_network` and `database_network` do not exist.

Wi-Fi control, BLE, USB, cellular control, and future communication mechanisms
are separate capabilities with separate APIs and lifecycles. They are not
hidden behind a universal `Network` trait or stuffed into the `ip_stack`
field.

Platform owns the execution work required to keep its IP service alive. System
and Plugins receive the usable IP stack handle, not its runner or
platform-specific setup objects.

## Logging and Agent tracing

Ordinary diagnostics across Platform, HAL, System, Core, and Plugins use the
`log` facade. Library and subsystem crates emit records; they do not choose an
output device, initialize a global logger, or depend on a Host-only logging
implementation.

The selected Platform installs the process- or firmware-global `log::Log`
backend in `Platform::prepare`, before it initializes any other Platform
mechanism. The concrete sink belongs to the Platform integration:

- macOS and Linux write formatted records to standard error;
- ESP32-C6 writes through its USB Serial/JTAG debug channel;
- STM32 writes through non-blocking RTT.

Host logger installation and sink selection live inside each concrete Host
Platform. They are not shared mechanisms and must not be moved into a
cross-Platform logging crate.

`BARRACUDA_LOG_LEVEL` selects `off`, `error`, `warn`, `info`, `debug`, or
`trace` at build time and defaults to `info`. Each concrete Platform build
script validates the value and generates a `log::LevelFilter` constant for its
backend. Platforms must not read or parse the setting at runtime; invalid
values are build errors.

Logging is process-global support, not a consumable business capability. It
does not become a `PlatformResources` field, Plugin capability, Workflow
contract, or Board peripheral. Platform-specific output setup remains outside
Core and Plugins.

`tracing` is not the general logging API. It remains available only to the
Agent subsystem where nested spans and inherited execution context justify its
cost and complexity. Agent trace lines may be rendered into the same `log`
sink, but ordinary Core and Plugin code must not acquire a `tracing`
dependency.

Logs must identify lifecycle boundaries and failures without including API
keys, credentials, message bodies, or other secret-bearing payloads.

## Bulk memory

Bulk memory is a process-global allocation domain for large plain-data buffers.
It is runtime support rather than a consumable business capability, so it does
not become a `PlatformResources` field, `PluginContext` field, Workflow
contract, or Board peripheral.

The Board matrix declares directly addressable external memory as a hardware
fact, including its technology, interface, and installed capacity. The selected
Platform owns initialization of that memory and installs the allocator used by
the shared bulk-memory mechanism. Platforms without a distinct external-memory
domain install their normal global allocator behind the same API.

Callers allocate through `BulkVec<T>` and `BulkBox<T>`. Their element boundary
requires `bytemuck::Pod`; the underlying allocator is private and cannot be
recovered by safe caller code. This keeps atomics, locks, futures, pointers,
and other control state out of external memory on chips where those values
cannot be accessed safely. Bulk containers remain explicit: the Platform does
not add external memory to the ordinary global allocator used by arbitrary
System and Plugin state.

A C library that allocates through `calloc`-style hooks asks for untyped
memory, so the `Pod` boundary cannot be checked for it. `bulk_memory::foreign`
gives such a library raw zeroed byte buffers from the same domain; its owner
sends only large plain-data buffers there and keeps the library's control
structures on the global heap. `shared/tls` is the one user: mbedTLS
allocations of 4 KiB or more (record buffers and certificate copies) go to bulk
memory, everything smaller to the global heap.

## TLS and HTTP clients

TLS is a shared software service above the Platform boundary. Its only
Platform-specific input is randomness, which the Platform already supplies as
`entropy`; the engine, its process lifetime, and the trust roots are the same
on every Platform.

`shared/tls` owns the TLS engine and the trust roots. `Tls::new(entropy)`
initializes the process-wide mbedTLS engine from the Platform's entropy source
and refuses `UnavailableEntropy` rather than fall back to a weaker generator; a
source that fails later fails the handshake with an entropy error.

The trust roots are Mozilla's CA store, pinned in `shared/tls/roots`, the same
on every Platform. As in ESP-IDF's certificate bundle, `build.rs` reduces each
root to its subject name and public key and compiles the result in (about
54 KiB for 121 roots). mbedTLS gets an empty CA chain and a verify callback:
the top certificate of a server chain arrives untrusted, and the callback
trusts it only when a bundled root of its issuer's name verifies its
signature. mbedTLS verifies the rest of the chain. The roots stay in flash and
only the matching root's key is parsed, so no Platform keeps parsed roots in
RAM. Each connection's record buffers, 16 KiB for incoming records (servers
send full-size records) and 4 KiB for outgoing ones, come from bulk memory (see
"Bulk memory"), which is external RAM on Boards that have it. That outgoing
size is not the prebuilt device libraries' configuration, so every device
Platform compiles mbedTLS and selects the C compiler for its chip through a
`barracuda-tls` feature. mbedTLS checks no certificate dates, so expired roots are left out of the
bundle when it is generated. `mbedtls-rs` is vendored in
`shared/tls/mbedtls-rs` with the verify callback and a fallible random source
added.

No Platform reads its operating system's certificate store, and `shared/tls`
reads no environment variables or certificate files at run time. Only a host
test build may add roots, through `BARRACUDA_TLS_TEST_ROOTS` at build time (for
a test network that intercepts TLS); device firmware refuses it. It also owns the few
C library functions mbedTLS calls on bare-metal targets. A Plugin must not
load certificates, initialize a TLS backend, or select a TLS implementation.

HTTP is a shared software service above the Platform boundary:

~~~text
PlatformResources { ip_stack, entropy }
                 |
                 v
System composition: barracuda_tls::Tls::new(entropy)
                 |
                 v
shared/http-client
        +-- ClientFactory (composition only)
        +-- Client request facade (consumer API)
        +-- buffered execute
        +-- streaming execute
                 |
                 +----> Agent model protocol adapter
                 +----> message-channel Providers
~~~

`shared/http-client` is the workspace's only reqwless integration. It owns URL
and header validation, connection reuse, request-body streaming, response-body
streaming, buffering, and transport error classification. Protocol crates may
adapt its domain-neutral request and response values, but they do not create a
second reqwless transport implementation.

System combines the Platform's `ip_stack` with the `Tls` it built into one
`http_client::ClientFactory`. When TLS cannot start, the factory has no TLS and
`https://` requests fail instead of falling back to plaintext. Construction code may clone that factory;
business consumers receive only `http_client::Client` and use its fluent
`get`/`post`/`request` facade. TCP, DNS, TLS configuration, reqwless types, and
buffer sizes must not parameterize Plugin or domain APIs.

Each `ClientFactory::create()` result independently owns connection reuse and
request serialization while all results share the Platform network pool. Thus
“one shared HTTP client” means one facade, implementation, and construction
policy, not one mandatory TCP connection for unrelated concurrent protocols.

Long-lived receive loops (a channel's long poll or push connection) never take
a connection from that request pool. System also builds
`http_client::ReceiveSlots` over the same IP stack, DNS resolver, and TLS
engine and places it in `PluginContext::receive_slots`. Each slot owns one
statically allocated socket; a channel holds a `ReceiveLease` while it
receives, and the lease gives either HTTP clients over its single connection
or a raw byte stream whose read half is cancel-safe, for protocols such as
WebSocket. The static pool has `http_client::RECEIVE_SLOTS` slots, the largest
any Target uses. The runtime capacity is a memory budget the selected Platform
declares in its manifest, because the Platform owns the heap and decides
whether Board-declared external memory becomes bulk memory:

~~~yaml
network:
  long-lived-connections:
    internal-memory: 1   # TLS record buffers in internal RAM
    external-memory: 3   # Board external memory installed as bulk memory
~~~

`TargetIdentity::long_lived_connections` picks the figure for the selected
Board, and System never creates more slots than that.

Embassy is the common task, timer, and lifecycle model used by Barracuda on
embedded, macOS, and Linux Platforms. The operating-system Platforms may use
native facilities behind their implementations, while System and Plugins retain
the same Embassy lifecycle.

Task ownership and cancellation are defined in
[`execution-ownership.md`](execution-ownership.md).

## System

System is the aggregation entry. Its responsibilities include:

- consuming the selected Target resources;
- constructing mounted filesystems and databases from partitions;
- constructing Plugin Manager;
- assembling the fixed Plugin set;
- supplying Platform services and HAL capabilities to their consumers;
- establishing Plugin registration and startup order.

System does not interpret a concrete Board's pins, instantiate peripheral
implementations, parse a vendor partition format, or expose the selected Platform type
through Plugin APIs.

Capabilities produced by HAL may be consumed directly by System or installed
as typed capabilities for Plugins. The choice follows ownership and lifecycle,
not the hardware type that produced the capability.

## Plugins

Plugins are system-managed functional modules. They consume semantic
capabilities and remain independent of Platform, Board, peripheral
implementation, and chip-driver types.

For example, a Display Plugin consumes Display. It does not consume SPI pins,
a Board matrix, or a concrete display implementation. A storage-consuming
Plugin receives scoped storage rather than a raw partition.

Guidance for choosing among typed Plugin capabilities, Workflow contracts, and
Agent Tools lives in [`plugin-communication.md`](plugin-communication.md).
Execution ownership for long-lived Plugin work lives in
[`execution-ownership.md`](execution-ownership.md).

### Plugin directory ownership

The framework owns the top-level namespace of `plugins/<id>/`. Plugin authors
organize custom content under `resources/`, leaving the root available for
framework-defined directories and future framework extensions. The framework
defines `plugin.toml`, `crates/`, `docs/`, `filesystem/`, and the `resources/`
container; authors choose the organization within that container.

~~~text
plugins/<id>/
├── plugin.toml
├── crates/
├── docs/
├── filesystem/
│   └── resources/       # bundled read-only runtime files
└── resources/          # author-owned source content and build inputs
    ├── web/
    ├── agent-tools/
    └── workflow-schema/
~~~

Web source and Plugin-owned tests live in `resources/web/`. Agent Tool
definitions and Workflow schemas belong under the
same author-owned container, grouped by their purpose. Consumers locate these
inputs through explicit paths or a documented resource contract; arbitrary
subdirectory names do not create framework behavior or Plugin dependencies.

Repository `resources/` is separate from the Plugin's runtime `/resources`
mount. Its contents are not automatically packaged. A Plugin's build process
places deployable outputs in `filesystem/resources/`, which the generic image
builder includes for selected Plugins. For example, Bun compiles
`resources/web/entry.ts` into `filesystem/resources/entry.js`. Each Plugin owns
its output; the portal shell loads independently registered entries on demand.

Custom author directories stay within `resources/`; adding one does not reserve
a new Plugin-root name. Existing root-level custom directories and crate-local
resource collections are migration work. Their consuming paths and resource
contracts must change together when they move.

Frontend development tools are repository-owned: one root `package.json`,
`bun.lock`, TypeScript configuration, formatter, and linter cover all Plugins.
Install dependencies once at the repository root. Run `bun run format`,
`bun run format:check`, `bun run lint`, `bun run check`, and `bun run test`
there. Tests remain with their owning Plugins; shared test helpers and the
ordinary browser-module builder live in `tools/web/`.

Each Plugin declares its build command, inputs, and isolated outputs in
`plugin.toml`. Ordinary web entries use the shared builder. A Plugin with
different output needs, such as the portal shell's HTML/CSS/JS scaffold, owns
its custom build script in `resources/web/`. Repository-wide development
commands are not repeated as per-Plugin tasks or package manifests. Shared
tooling does not merge Plugin assets or change image-selection semantics.

### Plugin host tasks

Plugins declare named host tasks in `plugin.toml`. Each task defines an
executable and literal arguments, a working directory relative to the Plugin
root, and optional input paths, output paths, and environment dependencies.
The host executor treats Bun, npm, generators, and other commands uniformly.
It executes trusted repository code with the developer's permissions and
environment; this is a build integration boundary, not a sandbox.

`cargo plugin run <task>` runs that task sequentially for enabled Plugins in
directory order. Plugins without the task are skipped. `--plugin <id>` selects
one enabled Plugin and requires the task to exist. A failure stops execution.
Plugin `depends-on` describes capability dependencies, not a host-task DAG.

Each Plugin with automatic resource generation installs a `build.rs` hook
calling the shared `barracuda-plugin-build` executor. Cargo invokes only its
`build` task before compiling the Plugin, including during `cargo build`,
`cargo run`, and `cargo check`. Hooks follow Cargo's actual package graph;
explicitly building a disabled package with `-p` still builds that package.
The selected System registry determines the normal application's Plugin graph.

Cargo watches the manifest, declared input and output paths, PATH, and declared
environment variables. When both inputs and outputs are declared, the hook
uses content fingerprints in Cargo's OUT_DIR to skip unchanged commands and
recreate missing or changed outputs. Inputs may include explicitly named
shared source paths; outputs and the working directory stay Plugin-relative.
Without explicit inputs, Cargo watches the working directory. Without both
input and output declarations, the command runs whenever the hook runs.
Watched paths are literal files/directories, not glob expressions; fingerprinted
trees contain ordinary files/directories rather than symlinks.

The CLI invokes tasks unconditionally, allowing explicit rebuilds after tool
updates. A per-Plugin host lock serializes manual execution with Cargo hooks.
Tasks inherit terminal interrupt behavior and must remain foreground processes.
Tool installation is an explicit developer action, never an implicit build step.
Automatic build tasks must not re-enter Cargo, which may hold its build lock.
Image collection consumes completed outputs; it is a separate operation and
does not depend on accidental ordering between Cargo build scripts.

## Dependency and repository direction

~~~text
platforms/
+-- api/
+-- selected/          # selects only Platform
+-- esp32/             # one Platform per ESP chip: services + vendor HAL adaptation
+-- esp32c3/
+-- esp32c6/
+-- esp32p4/
+-- esp32s3/
+-- stm32/             # Platform services + vendor HAL adaptation
+-- nrf/
+-- ch/
+-- linux/
+-- macos/
+-- virtual-io/        # host-only virtual GPIO/I2C shared by linux and macos

peripherals/
+-- api/
+-- config/
+-- impl/<peripheral>/<implementation-id>/

drivers/
+-- chips/<chip>/

boards/
+-- api/
+-- hal/
+-- config/
+-- configs/<board>/
+-- selected/          # selects only Board + Board HAL
+-- <board-related composition crates>/

composition/
+-- api/               # separate Platform and Board HAL resource fields
+-- selected/          # generated allocation and orchestration of both axes

core/
+-- plugin/
|   +-- crates/
|       +-- api/
|       +-- manager/
|       +-- macros/
|       +-- manifest/
+-- system/

plugins/
+-- <plugin>/
~~~

Dependencies flow toward semantic consumers:

~~~text
Platform adaptation + peripheral implementations + Board matrix -> Board HAL ----+
                                                                         |
Platform services -> { IP, entropy, partitions } ------------------------+-> System -> Plugins
~~~

Platform crates do not own peripheral implementations or Board
composition. They do own their vendor-HAL token vocabulary and GPIO, I2C, SPI,
camera, I2S, and delay constructors. DMA-backed media resources are composed
statically: `camera-capture` and `i2s-stream` declarations size storage with
`dma-buffer-bytes`, and the selected Platform allocates it at the concrete Board
call site. Semantic adaptation remains under `peripherals/impl`; reusable
register and protocol behavior belongs under `drivers/chips`. Board-related
crates remain under `boards/`; the selected Target composition root remains
outside an individual Platform implementation.

Generated composition calls the selected Platform's `hal` module and hands the
resulting `embedded-hal` resources to the peripheral implementation selected by
its manifest. A new Board reusing existing implementations needs only Board YAML.
A new implementation adds its crate and `peripheral.yml`; it may reuse an
existing chip driver and adds no central renderer branch.

## Static composition requirements

- Platform and Board are independently selected at build time.
- Board HAL is statically composed from one Platform HAL, one Board matrix, and
  its peripheral implementations.
- The concrete Platform and Board HAL are monomorphized for one Target.
- Runtime Platform lookup and a dyn Platform registry are unnecessary.
- Peripheral implementation calls remain statically dispatched.
- A device entry acquires the hardware singleton once, constructs the selected
  Target bindings, and invokes selected Target composition. Host Targets
  take their Platform's virtual peripheral singleton the same way.
- Board HAL initialization consumes owned bindings; it does not reacquire
  peripherals or resolve pin numbers at runtime.
- The Board HAL constructs only the optional peripherals and exposed I/O
  explicitly declared by the Board config.
- The common framework defines no conflict, disjointness, or mux policy between
  those declarations.
- Digital GPIO, PWM, I2C, and SPI operations use `embedded-hal` ecosystem
  contracts. Barracuda adds only GPIO mode configuration and analog conversion
  contracts that `embedded-hal` 1.0 does not provide.
- Shared I2C/SPI buses use upstream bus-device adapters and one statically
  allocated owner.
- Platform runners and peripheral implementation runners stay with their respective owners.
- System and Plugin hot paths do not perform Platform, Board, or implementation lookup.
- Adding a Plugin does not change Platform API.
- Adding a peripheral does not change Platform API.
- Adding a Board composes existing Platform, chip-driver, and peripheral-implementation building blocks without
  copying either implementation.

## Review checklist

Before changing target-sensitive code, verify:

1. Does Platform own ESP/STM32/nRF/CH/Linux/macOS-level mechanisms and adapt
   its vendor HAL into the upstream contracts consumed by chip drivers and peripheral implementations?
2. Is concrete product hardware described by Board rather than Platform?
3. Is each peripheral API under `peripherals/api`, each semantic implementation
   under `peripherals/impl/<peripheral>/<implementation-id>`, and each reusable
   low-level driver under `drivers/chips/<chip>`?
4. Does Board HAL composition use the selected Platform HAL plus Board matrix
   and peripheral implementations?
5. Does Platform expose one partitions collection instead of business-specific
   partition fields?
6. Are IP, Wi-Fi control, BLE, USB, and other communication capabilities named
   exactly instead of being hidden behind a generic Network abstraction?
7. Are LittleFS, FATFS, and ekv constructed by System rather than Platform?
8. Can a Plugin or peripheral be added without changing Platform API?
9. Does a Plugin consume semantic peripherals instead of Board, implementation, chip driver, or raw
   partition types?
10. Are Platform and Board selected independently, with compatibility checked
    only at Target composition?
11. Does a Display flow from Board matrix plus Display implementation into HAL, then to
    System or a Plugin?
12. Does the application obtain the complete selected Target from the target
    composition crate without performing the wiring itself?
13. Does System build TLS from the Platform's entropy and the shared trust
    roots, with no Platform touching TLS, while all HTTP consumers use
    `shared/http-client`?
14. Does the device entry acquire the hardware singleton exactly once and pass
    the resulting binding pair through selected-target composition before
    returning Target resources?
15. Are optional peripherals and their private wiring declared under the Board
    config's `peripherals` field?
16. Are exposed digital GPIO, analog, PWM, I2C, and SPI interfaces explicitly
    declared instead of inferred from unused hardware?
17. Does the common layer avoid imposing conflict or mux policy on declarations
    that reference the same physical resource?
18. Can the capability expose an `embedded-hal`, `embedded-hal-async`, or
    domain ecosystem contract directly instead of introducing a Barracuda
    operation trait?
19. Does an SPI implementation receive `SpiDevice` when it owns a CS-selected device,
    with sharing and per-device configuration handled by an upstream adapter?
