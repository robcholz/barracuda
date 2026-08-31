# Platform Architecture

This document defines Barracuda's target architecture. It is authoritative for
the meaning and ownership of Platform, Board, Driver, HAL, Target, System, and
Plugin. Current code that contradicts these boundaries is migration work, not
precedent.

## Core model

Barracuda has two independently selected axes:

~~~text
Target = Platform + HAL
HAL    = Board matrix + peripheral Drivers
~~~

- **Platform** is an execution platform family: ESP, STM32, nRF, CH, Linux,
  macOS, or an equivalent environment. It provides platform mechanisms such as
  an IP stack, TLS, and partitions. It does not describe a Board's concrete
  peripherals, wiring, or product hardware matrix.
- **Board** is one concrete hardware combination. Its matrix describes the
  chip, buses, pins, clocks, attached peripherals, fixed wiring, and native
  physical layout of that product.
- **Driver** means a reusable peripheral driver. Display controllers, sensors,
  touch controllers, and external storage devices are Drivers. Network is a
  Platform service in this architecture; it is not classified as a peripheral
  Driver.
- **HAL** is the statically composed result of applying peripheral Drivers to a
  Board matrix. It exposes initialized hardware capabilities such as Display
  for System or Plugins to consume.
- **Target** is the independently selected Platform combined with the selected
  Board's HAL.
- **System** is the aggregation entry. It constructs system-owned services,
  assembles the fixed Plugin graph, and routes Platform and HAL capabilities to
  their consumers.
- **Plugin** is a self-contained functional module managed by System.

A Platform is not a Board support package, a peripheral-driver collection, a
HAL, or a System dependency bag.

## Composition boundary

Board and Platform selection are independent build inputs. A Board never
selects its Platform through a Rust associated type, Cargo dependency, or YAML
reference.

~~~text
one hardware bootstrap --------+------------------------------+
                               |                              |
selected Platform -------------+-> Platform bindings -> Platform
                                                              |
selected Board config -> generated Board composition          |
  +-- built-in Driver + wiring -> built-in capabilities       |
  `-- explicitly exposed I/O  -> GPIO/I2C/SPI capabilities    |
                               |                              |
                               +------------> HAL ------------+
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
Platform and Board can form one Target. At boot it acquires the hardware
singleton, invokes the generated Platform and Board constructors, and returns
their resources to System without flattening one axis into the other. The
composition follows the selected Board declarations; it does not infer an I/O
surface from hardware that the Board config omitted. Application entries do not
parse YAML, import a concrete Platform, acquire chip peripherals, instantiate
peripheral Drivers, or wire Board peripherals.

~~~rust,ignore
let resources = barracuda_target::resources(spawner).await?;
let system = System::new(lanes, resources).await?;
~~~

The returned shape preserves ownership:

~~~rust,ignore
TargetResources {
    platform: PlatformResources { ip_stack, tls, partitions },
    board_hal: BoardHalResources { /* semantic capabilities */ },
}
~~~

Board HAL capabilities never become fields of `PlatformResources`. Platform
services never become fields of a Board HAL resource bundle.

## Platform

A Platform represents a family such as ESP, STM32, nRF, CH, Linux, or macOS.
It owns integration with that family's execution environment and supplies
platform-level services. Concrete Board peripherals do not become Platform
fields or Platform associated types.

Platform resources have stable, exact shapes. The current common contract
exposes one Embassy IP stack, one TLS client capability, and one partitions
collection:

~~~rust,ignore
pub struct PlatformResources<Tls, Partitions> {
    pub ip_stack: embassy_net::Stack<'static>,
    pub tls: Tls,
    pub partitions: Partitions,
}
~~~

This is architectural guidance rather than a frozen Rust signature. The
invariant is that partitions remain a collection. Business roles never become
fields such as filesystem_partition, web_assets_partition, or
database_partition. The IP capability likewise remains `ip_stack` rather than
web_network or database_network. TLS remains an independent `tls` capability;
it is not hidden inside `ip_stack` or reconstructed by an HTTP consumer.

Adding a Plugin, peripheral, filesystem, database, or application subsystem
does not modify the Platform API or PlatformResources shape.

Platform initialization may start tasks required by its own services. It does
not start tasks belonging to a peripheral Driver. Driver lifecycle belongs to
the HAL composition that owns that Driver.

## Board

A Board describes one concrete product hardware matrix:

- exact chip and package;
- buses, pins, DMA channels, interrupts, and clocks;
- attached peripheral models and their fixed wiring;
- explicitly exposed GPIO, I2C, and SPI connectors;
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
also accepts an explicit Board name for automation. The ordinary Cargo build
reads that state; it does not require an environment variable or a Board-aware
build wrapper.
`boards/selected` must not inspect `target_os` or `target_arch`, and the outer
Target composition must not infer a default Board or activate a Board feature
from them. A missing selection is a build error that points back to the select
command. This keeps target policy out of Board parsing and code generation
while preserving Board and Platform as independent inputs.

## Board-declared hardware surface

The selected Board config is the source of truth for the hardware surface that
Barracuda constructs. It contains two independent sets of declarations:

- built-in peripherals name a Driver and the pins, bus, geometry, polarity,
  and other fixed inputs used to construct it;
- exposed I/O names each digital GPIO, analog channel, PWM channel, I2C, or SPI
  capability and the concrete pins and controller used to construct it.

The generated Board composition constructs exactly those declarations. It does
not expose unused pins or peripheral instances automatically. A physical
resource omitted from `exposed-io` is absent from the Barracuda I/O surface even
when the chip could otherwise use it.

The common Board model does not assign conflict or mux semantics to references
shared by declarations. A concrete Board config may deliberately describe
hardware muxing or overlapping uses when its hardware and adapter support them.
Barracuda neither promises that those uses can operate simultaneously nor
rejects the configuration solely because the same physical identifier appears
more than once. Electrical correctness, mode transitions, and runtime
coordination remain properties of that concrete Board composition.

Generic validation covers the Board schema, referenced Driver identifiers,
required wiring fields, and whether the selected adapter can construct the
declaration. It does not certify the resulting product wiring or resource-use
policy.

## Exposed I/O and built-in peripherals

Board configuration distinguishes fixed product hardware from connectors that
Barracuda intentionally makes available to application-level consumers. The
shape is conceptually:

~~~yaml
builtin-peripherals:
  status-indicator:
    driver: gpio-output-indicator
    bindings:
      pin: gpio10
    parameters:
      active-low: true
  display:
    driver: mipidsi-st7789
    bindings:
      spi: spi2
      chip-select: gpio8
      data-command: gpio9
    parameters:
      width: 320
      height: 240

exposed-io:
  gpio:
    user-button:
      pin: gpio2
  analog-input:
    sensor-voltage:
      peripheral: adc1
      pin: gpio3
      channel: 2
  pwm:
    actuator:
      peripheral: ledc0
      pin: gpio4
      channel: 0
  i2c:
    expansion:
      peripheral: i2c0
      scl: gpio4
      sda: gpio5
      frequency-hz: 400000
  spi:
    expansion:
      peripheral: spi0
      sck: gpio18
      mosi: gpio19
      miso: gpio20
      frequency-hz: 20000000
~~~

The concrete schema may use chip-native names, but it stores hardware facts,
Driver identifiers, electrical configuration, safe boot state, and stable
connector names. Rust crate paths, Platform types, Plugin identities, and
application policy do not belong in the Board matrix. A Driver identifier
selects registered composition code; the YAML does not name a Rust type or
crate path.

Board HAL resources retain the distinction between the two classes:

~~~rust,ignore
BoardHalResources {
    builtins: BuiltinCapabilities { /* display, indicator, ... */ },
    io: ExposedIo { gpio, i2c, spi },
}
~~~

`BoardHalResources<Builtins, Io>` is the common ownership envelope. Its generic
fields preserve the concrete, statically composed capability types for each
selected Board. Built-in Drivers return semantic capabilities such as Display
or Indicator. Exposed declarations return I/O capabilities. The envelope does
not imply that the underlying physical resources are disjoint. System routes
each resulting capability to the subsystem or Plugin that owns its behavior.

`ExposedIo` is an ownership and naming envelope, not a parallel I/O trait
family. Its entries retain concrete, statically dispatched values implementing
the upstream contracts:

| Hardware use | Upstream contract |
| --- | --- |
| dynamically configured digital GPIO | Barracuda `ConfigurableDigitalPin`, with operations from `embedded_hal::digital` |
| asynchronous GPIO edge wait | `embedded_hal_async::digital::Wait` |
| analog input or output | Barracuda `AnalogInput` or `AnalogOutput` |
| PWM output | `embedded_hal::pwm::SetDutyCycle` |
| I2C controller or shared handle | `embedded_hal::i2c::I2c` or `embedded_hal_async::i2c::I2c` |
| exclusively owned SPI controller | `embedded_hal::spi::SpiBus` or `embedded_hal_async::spi::SpiBus` |
| one device on an SPI controller | `embedded_hal::spi::SpiDevice` or `embedded_hal_async::spi::SpiDevice` |

Barracuda does not replace the digital GPIO, PWM, I2C, or SPI operations that
`embedded-hal` already defines. `ConfigurableDigitalPin` adds the missing
runtime transition between disabled, input, and output states; the configured
value continues to implement `InputPin`, `OutputPin`, and
`StatefulOutputPin`. `AnalogInput` and `AnalogOutput` cover ADC and DAC because
`embedded-hal` 1.0 has no corresponding contracts. Initialization, pin mux,
stable Board names, and static construction come from the concrete Board
adapter. Cross-declaration conflict policy is not part of this common layer.

Exposed I/O returns capabilities with stable Board-level names. A digital GPIO
owner selects input bias or output drive and initial level at runtime. An analog
capability binds one controller channel and pin. An I2C capability owns its
controller and signal pins. An SPI capability owns its controller and signal
pins; a separately exposed GPIO supplies application-managed chip select when
needed. When another Board declaration references the same physical resource,
the concrete Board composition defines how those logical capabilities relate.

Reusable Drivers consume the standard `embedded-hal` and
`embedded-hal-async` contracts where those contracts express the operation.
The chip HAL's own type erasure is used when a bounded collection needs a
uniform concrete type: for example, ESP and Embassy STM32 provide type-erased
pin representations and erase peripheral instance identity inside their I2C
and SPI drivers. Barracuda adapters do not reproduce that machinery. The
selected build keeps concrete storage and dispatch statically allocated;
consumers do not look up a Platform, Board, or Driver implementation at
runtime, and async HAL traits are not converted into `dyn` trait objects.

`embedded-hal` does not standardize changing a GPIO between input and output
modes. Every exposed digital GPIO therefore implements the small Barracuda
configuration contract in addition to the upstream operation traits. Protocol
alternate functions are established by constructing the declared controller;
they are not another mode on this common digital API. Each chip adapter maps
the configuration contract to its stable flexible-pin mechanism.

Built-in buses use the existing upstream sharing adapters. HAL-owned buses use
the blocking or async `embassy-embedded-hal` shared I2C/SPI devices appropriate
for the concrete Driver and Embassy mutexes. Devices with different bus
configurations use its per-device configuration support when the chip HAL
provides `SetConfig`; otherwise a small chip adapter delegates configuration to
the vendor driver's stable API. The corresponding `embedded-hal-bus` adapters
are also used where their ownership and execution model is the closer fit.
Driver code receives `SpiDevice` when it addresses a CS-selected device and
`SpiBus` only when it truly owns the whole bus.

A built-in display uses an existing controller Driver when available. For a
blocking MIPI DCS transport, controllers such as ST7789 are composed with
`mipidsi`: its SPI interface consumes `embedded_hal::spi::SpiDevice`, and
drawing consumers use the resulting
`embedded-graphics-core::DrawTarget` implementation. A DMA or async display
path selects an existing Driver that exposes that execution model, or isolates
a blocking Driver behind its HAL-owned task. A Barracuda Display facade is
introduced only when System needs lifecycle, power, backlight, framebuffer, or
concurrency semantics that those upstream contracts do not express.

System assigns every exposed port collection to one owner. That owner may
publish a higher-level typed capability or Event Router contract for scripts
and Plugins. Arbitration, access control, and dynamic device attachment belong
to that owner. Any physical overlap already declared by the Board remains a
property of the concrete Board composition.

The Lua GPIO, I2C, and SPI Plugins are such owners. Board HAL still returns a
concrete, move-only exposed-I/O value; it has no scripting service contract and
adds no shared ownership or lock. Selected-target composition consumes that
value and adapts only the explicitly exposed resources at the Lua boundary.
`PluginContext` is a construction-time handoff: each hardware Plugin calls its
corresponding `take_*` operation once and becomes the sole owner of that boxed
adapter value.

The Plugin then places its owned value behind an Embassy async mutex inside
the Lua package. Cloned `Arc`s share only that Lua-layer lock so concurrent Lua
callbacks can reach the same package-owned value; they do not clone or
reconstruct hardware. Adapter operations take `&mut self`, and the mutex guard
is held across the returned future, making the exclusivity required by the
underlying `embedded-hal` or `embedded-hal-async` value explicit. This dynamic
adapter exists only because logical names and Lua callbacks are runtime data.
HAL, Board composition, built-in peripheral Drivers, and non-Lua consumers
remain concrete and statically dispatched.

## Peripheral Drivers and HAL

Drivers implement reusable peripheral behavior. The Board matrix supplies the
concrete bus and wiring values used to instantiate them:

~~~text
Board matrix + peripheral Drivers -> HAL capabilities
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
Display peripheral Driver
             |
             v
Display capability
             |
             +----> System
             +----> Display-consuming Plugin
~~~

The consumer receives the semantic Display capability. It does not reconstruct
the Driver from raw pins or import the concrete Board crate.

Adding a Display changes the Board matrix, peripheral-driver composition, and
the System or Plugin wiring that consumes Display. It does not change Platform
or PlatformResources.

Driver tasks and interrupt-facing state are owned by the HAL composition that
instantiated the Driver. System and Plugins receive handles or semantic
capabilities; they do not own the Driver runner.

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
The selected Target composition produces both `Platform::Bindings` and
`BoardHal::Bindings`, so compatibility is checked at the only layer that
imports both selected axes. Neither axis names or selects the other.

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
usable regions through partitions. The collection supports arbitrary entries;
its Rust type does not grow a field for every consumer.

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
+-- region selected by System -> mount LittleFS
+-- region selected by System -> mount read-only FATFS
+-- region selected by System -> open ekv
+-- remaining regions         -> OTA, boot state, or future consumers
~~~

Platform does not know LittleFS, FATFS, ekv, Event Router, WebServer, Plugin
Manager, or Plugin identities. System owns those choices and constructions.

A Plugin normally receives scoped semantic storage from System. If a product
requires a new dedicated physical region, its native Board layout gains that
region and System consumes it from partitions; the Platform API remains
unchanged.

## Filesystems and database

System constructs software storage from Platform partitions. Filesystem and
database objects are not Platform resources.

Different filesystems retain their native APIs:

- mutable runtime files use the selected LittleFS implementation;
- provisioned read-only Web assets use the selected FAT implementation;
- key/value state uses ekv over its raw writable region.

Barracuda does not force LittleFS and FATFS through one universal filesystem
trait. Their different APIs and guarantees remain visible to the System-owned
consumer that selected them.

~~~text
PlatformResources::partitions
             |
             v
System storage construction
+-- LittleFS -> Event Router and file-oriented consumers
+-- FATFS    -> WebServer static assets
+-- ekv      -> Plugin Manager scoped storage
~~~

System mounts the process-wide VFS before constructing its consumers. Event
Router accesses that global namespace directly and owns
`/system/workflows.json`, a single JSON array containing its ordered Workflow
definitions. `/system` is a shared System namespace; Event Router does not own
sibling paths.
WebServer owns URL-to-asset behavior over the read-only FAT filesystem. Plugin
Manager owns ekv namespaces and exposes only semantic Plugin storage.

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
does not become a `PlatformResources` field, Plugin capability, Event Router
contract, or Board peripheral. Platform-specific output setup remains outside
Core and Plugins.

`tracing` is not the general logging API. It remains available only to the
Agent subsystem where nested spans and inherited execution context justify its
cost and complexity. Agent trace lines may be rendered into the same `log`
sink, but ordinary Core and Plugin code must not acquire a `tracing`
dependency.

Logs must identify lifecycle boundaries and failures without including API
keys, credentials, message bodies, or other secret-bearing payloads.

## TLS and HTTP clients

TLS is a Platform capability because each Platform owns its randomness,
trust-root source, TLS engine initialization, and process or firmware lifetime.
Host Platforms load the system certificate bundle. Device Platforms construct
the same semantic capability from Platform RNG state and provisioned DER trust
roots. A Plugin must not load Host certificates, initialize a TLS backend, or
select a TLS implementation through a Host-only feature.

`shared/tls` owns only TLS mechanisms: the `ClientTls` contract and constructors
that accept caller-supplied RNG and DER/PEM roots. It must not inspect
environment variables, read certificate files, or contain operating-system CA
paths. Those policies live in each concrete Host Platform.

HTTP is a shared software service above the Platform boundary:

~~~text
PlatformResources { ip_stack, tls }
                 |
                 v
System composition
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

System combines the Platform's `ip_stack` and `tls` capabilities into one
`http_client::ClientFactory`. Construction code may clone that factory;
business components receive only `http_client::Client` and use its fluent
`get`/`post`/`request` facade. TCP, DNS, TLS configuration, reqwless types, and
buffer sizes must not parameterize Plugin or domain APIs.

Each `ClientFactory::create()` result independently owns connection reuse and
request serialization while all results share the Platform network pool. Thus
“one shared HTTP client” means one facade, implementation, and construction
policy, not one mandatory TCP connection for unrelated concurrent protocols.

Embassy is the common task, timer, and lifecycle model used by Barracuda on
embedded, macOS, and Linux Platforms. The operating-system Platforms may use
native facilities behind their implementations, while System and Plugins retain
the same Embassy lifecycle.

The boundary between Event Router Components and owner-managed Embassy tasks is
defined in [`execution-ownership.md`](execution-ownership.md).

## System

System is the aggregation entry. Its responsibilities include:

- consuming the selected Target resources;
- constructing mounted filesystems and databases from partitions;
- constructing Event Router and Plugin Manager;
- assembling the fixed Plugin set;
- supplying Platform services and HAL capabilities to their consumers;
- establishing Plugin registration and startup order.

System does not interpret a concrete Board's pins, instantiate peripheral
Drivers, parse a vendor partition format, or expose the selected Platform type
through Plugin APIs.

Capabilities produced by HAL may be consumed directly by System or installed
as typed capabilities for Plugins. The choice follows ownership and lifecycle,
not the hardware type that produced the capability.

## Plugins

Plugins are system-managed functional modules. They consume semantic
capabilities and remain independent of Platform, Board, and peripheral Driver
types.

For example, a Display Plugin consumes Display. It does not consume SPI pins,
a Board matrix, or a concrete display-controller Driver. A storage-consuming
Plugin receives scoped storage rather than a raw partition.

Guidance for choosing between Event Router contracts and typed Plugin
capabilities lives in [`plugin-communication.md`](plugin-communication.md).
Execution ownership for long-lived Plugin work lives in
[`execution-ownership.md`](execution-ownership.md).

## Dependency and repository direction

~~~text
platforms/
+-- api/
+-- selected/          # selects only Platform
+-- esp/
+-- stm32/
+-- nrf/
+-- ch/
+-- linux/
+-- macos/

drivers/
+-- <peripheral>/

boards/
+-- api/
+-- hal/
+-- chips/<chip>/      # vendor-HAL token vocabulary and package catalog
+-- config/
+-- configs/<board>/
+-- selected/          # selects only Board + Board HAL
+-- <board-related composition crates>/

composition/
+-- api/               # separate Platform and Board HAL resource fields
+-- selected/          # generated allocation and orchestration of both axes

core/
+-- system/
+-- plugin-manager/

plugins/
+-- <plugin>/
~~~

Dependencies flow toward semantic consumers:

~~~text
peripheral Drivers + Board matrix -> HAL capabilities -------------------+
                                                                         |
Platform -> Platform resources { IP, TLS, partitions } ------------------+-> System -> Plugins
~~~

Platform crates do not own peripheral Driver implementations or Board
composition. Board-related crates remain under boards/; peripheral Drivers
remain under drivers/; the selected Target composition root remains outside an
individual Platform implementation.

A chip/package adapter supplies the vendor-HAL token vocabulary and
constructors used by generated composition. It contains neither product wiring
nor peripheral Driver behavior. It is an implementation dependency shared by
the selected Platform and Board HAL, not a third selection axis and not a
runtime capability exposed to System.

## Static composition requirements

- Platform and Board are independently selected at build time.
- HAL is statically composed from one Board matrix and its peripheral Drivers.
- The concrete Platform and HAL are monomorphized for one Target.
- Runtime Platform lookup and a dyn Platform registry are unnecessary.
- Peripheral Driver calls remain statically dispatched.
- The selected Target acquires the hardware singleton and invokes the generated
  constructors for the selected Platform and Board.
- Board HAL initialization consumes owned bindings; it does not reacquire
  peripherals or resolve pin numbers at runtime.
- The Board HAL constructs only the built-in peripherals and exposed I/O
  explicitly declared by the Board config.
- The common framework defines no conflict, disjointness, or mux policy between
  those declarations.
- Digital GPIO, PWM, I2C, and SPI operations use `embedded-hal` ecosystem
  contracts. Barracuda adds only GPIO mode configuration and analog conversion
  contracts that `embedded-hal` 1.0 does not provide.
- Shared I2C/SPI buses use upstream bus-device adapters and one statically
  allocated owner.
- Platform runners and Driver runners stay with their respective owners.
- System and Plugin hot paths do not perform Platform, Board, or Driver lookup.
- Adding a Plugin does not change Platform API.
- Adding a peripheral does not change Platform API.
- Adding a Board composes existing Platform and Driver building blocks without
  copying either implementation.

## Review checklist

Before changing target-sensitive code, verify:

1. Is Platform used only for ESP/STM32/nRF/CH/Linux/macOS-level mechanisms?
2. Is concrete product hardware described by Board rather than Platform?
3. Is an external peripheral implementation under drivers/?
4. Is HAL composition expressed as Board matrix plus peripheral Drivers?
5. Does Platform expose one partitions collection instead of business-specific
   partition fields?
6. Are IP, Wi-Fi control, BLE, USB, and other communication capabilities named
   exactly instead of being hidden behind a generic Network abstraction?
7. Are LittleFS, FATFS, and ekv constructed by System rather than Platform?
8. Can a Plugin or peripheral be added without changing Platform API?
9. Does a Plugin consume semantic capabilities instead of Board, Driver, or raw
   partition types?
10. Are Platform and Board selected independently, with compatibility checked
    only at Target composition?
11. Does a Display flow from Board matrix plus Display Driver into HAL, then to
    System or a Plugin?
12. Does the application obtain the complete selected Target from the target
    composition crate without performing the wiring itself?
13. Does Platform initialize TLS from Platform-owned randomness and trust
    roots, while all HTTP consumers use `shared/http-client`?
14. Does selected-target composition acquire the hardware singleton and invoke
    the generated constructors before returning Target resources?
15. Are built-in Drivers and their wiring declared by the Board config?
16. Are exposed digital GPIO, analog, PWM, I2C, and SPI capabilities explicitly
    declared instead of inferred from unused hardware?
17. Does the common layer avoid imposing conflict or mux policy on declarations
    that reference the same physical resource?
18. Can the capability expose an `embedded-hal`, `embedded-hal-async`, or
    domain ecosystem contract directly instead of introducing a Barracuda
    operation trait?
19. Does an SPI Driver receive `SpiDevice` when it owns a CS-selected device,
    with sharing and per-device configuration handled by an upstream adapter?
