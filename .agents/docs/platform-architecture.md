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
  an IP stack and partitions. It does not describe a Board's concrete
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
selected Platform YAML -------------------------+
                                                 |
selected Board YAML -> Board matrix              |
                         |                       |
                         v                       v
               peripheral Drivers -> HAL     Platform
                         |                       |
                         +-----------+-----------+
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
Platform and Board can form one Target. It invokes Platform initialization and
the Board HAL composition independently, then returns their resources to
System without flattening one axis into the other. Application
entries do not parse YAML, import a concrete Platform, instantiate peripheral
Drivers, or wire Board peripherals.

~~~rust,ignore
let resources = barracuda_target::resources(spawner).await?;
let system = System::new(lanes, resources).await?;
~~~

The returned shape preserves ownership:

~~~rust,ignore
TargetResources {
    platform: PlatformResources { ip_stack, partitions },
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
exposes one Embassy IP stack and one partitions collection:

~~~rust,ignore
pub struct PlatformResources<Partitions> {
    pub ip_stack: embassy_net::Stack<'static>,
    pub partitions: Partitions,
}
~~~

This is architectural guidance rather than a frozen Rust signature. The
invariant is that partitions remain a collection. Business roles never become
fields such as filesystem_partition, web_assets_partition, or
database_partition. The IP capability likewise remains `ip_stack` rather than
web_network or database_network.

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
- external memories and storage devices;
- native boot and physical layout artifacts;
- product-specific hardware feature presence.

Board YAML and Board-related crates live under boards/. A Board matrix is data
used to construct a HAL; it does not implement or select a Platform.

Board configuration identifies concrete hardware and the native names required
to construct it. It does not contain mounted filesystems, database objects,
Plugin instances, or application services.

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
+-- config/
+-- configs/<board>/
+-- selected/          # selects only Board + Board HAL
+-- <board-related composition crates>/

composition/
+-- api/               # separate Platform and Board HAL resource fields
+-- selected/          # thin orchestration of both selected axes

core/
+-- system/
+-- plugin-manager/

plugins/
+-- <plugin>/
~~~

Dependencies flow toward semantic consumers:

~~~text
peripheral Drivers + Board matrix -> HAL capabilities --+
                                                         |
Platform -------------------------> Platform resources --+-> System -> Plugins
~~~

Platform crates do not own peripheral Driver implementations or Board
composition. Board-related crates remain under boards/; peripheral Drivers
remain under drivers/; the selected Target composition root remains outside an
individual Platform implementation.

## Static composition requirements

- Platform and Board are independently selected at build time.
- HAL is statically composed from one Board matrix and its peripheral Drivers.
- The concrete Platform and HAL are monomorphized for one Target.
- Runtime Platform lookup and a dyn Platform registry are unnecessary.
- Peripheral Driver calls remain statically dispatched.
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
