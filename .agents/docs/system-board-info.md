# System Info and Board Info

This document defines the ownership boundary and public model for Barracuda's
System Info and Board Info abstractions. Both are read-only views assembled by
System from source-owned facts. The corresponding Plugins publish those views;
they do not discover hardware or inspect concrete Platform implementations.

## Scope

**System Info** describes the running Barracuda image and this boot. It answers
which software is running, on which execution Platform, and how long the
current boot has been alive.

**Board Info** describes the selected physical product. It answers which Board
matrix was built into the Target and exposes stable product and hardware facts
declared by that matrix.

The two views remain separate because an execution Platform can host many
Boards and a Board can be revised without changing the software release.

```text
platforms/<platform> ---- static Platform descriptor -----+
build/composition ------- image descriptor ---------------+--> System Info
System clock ------------ boot-relative observation ------+
                                                         System
boards/configs/<board> --- generated Board descriptor -----+--> Board Info
```

## Ownership boundary

Facts are declared by the layer that can state them authoritatively:

| Fact | Owner | Examples |
| --- | --- | --- |
| Image identity | composition/build | product name, semantic version, source revision |
| Platform identity | `platforms/` | Platform family, target architecture, operating environment |
| Boot-relative state | System | uptime and a process-local boot identifier |
| Product hardware | `boards/` | Board name, vendor, model, revision, chip, fixed hardware features |
| Peripheral runtime state | Board HAL or Driver | link state, sensor value, display state |

Platform identity is a small, static `PlatformInfo` descriptor exposed by the
Platform contract. Every concrete Platform supplies it, and
`platforms/selected` re-exports the selected descriptor. It is not a
`PlatformResources` field: querying identity requires no initialized service
and does not alter the stable IP, TLS, and partitions resource bundle.

Board identity extends the existing generated `Board` value. Board YAML is the
authoritative source, `boards/config` validates and generates it, and
`boards/selected` re-exports it. The Board Info Plugin consumes the generated
descriptor; it never parses YAML and never imports a concrete Board crate.

Composition combines the selected Platform descriptor, selected Board
descriptor, and image descriptor into immutable target metadata and passes it
to System beside `TargetResources`. This metadata preserves the independent
Platform and Board axes instead of flattening them into a generic fact map.
System records the boot epoch while constructing its Plugin context.

## Data model

All core descriptors are `no_std`, allocation-free, borrow static text, and
use fixed-shape typed fields. RPC DTOs own bounded text where the Event Router
wire contract requires it.

### System Info

The System Info snapshot has three sections:

```text
SystemInfo
+-- image
|   +-- name
|   +-- version
|   +-- source_revision: optional
|   `-- build_profile
+-- platform
|   +-- name
|   +-- family
|   +-- architecture
|   `-- environment
`-- boot
    +-- id
    `-- uptime
```

`image.name` and `image.version` identify the assembled Barracuda application,
not an individual crate. `source_revision` is included only when the build
pipeline provides it. Builds do not synthesize wall-clock timestamps, keeping
otherwise identical images reproducible. `build_profile` is a small enum such
as development, release, or profiling rather than an unconstrained Cargo
environment dump.

`PlatformInfo` uses stable semantic names. `family` distinguishes families
such as ESP, STM32, Linux, and macOS; `architecture` names the compiled CPU
architecture; `environment` distinguishes bare-metal and hosted execution.
It does not include Platform settings, filesystem paths, network interface
names, partition names, or implementation type names.

`boot.id` is unique only within the lifetime and persistence policy of the
running product. System may generate it from an injected entropy capability or
use a monotonic boot counter supplied by product composition. Callers must not
treat it as a device identity. `uptime` is derived from Embassy's monotonic
clock and is represented as integer milliseconds with saturating conversion at
the wire boundary.

Reset cause, heap statistics, task counts, temperatures, network addresses,
and storage usage belong to separate typed diagnostic capabilities when they
are introduced. Their availability and sampling semantics differ across
Platforms, so they do not become optional catch-all fields in System Info.

### Board Info

The Board descriptor contains stable, build-selected hardware facts:

```text
BoardInfo
+-- name
+-- vendor: optional
+-- model
+-- revision: optional
+-- hardware
|   +-- chip
|   `-- features: static list
`-- native_layout_kind
```

`name` is the stable Barracuda Board bundle identifier. `vendor`, `model`, and
`revision` are product-facing identity. `hardware.chip` retains the canonical
HAL chip name used for Platform compatibility checks. `features` is a static
list of semantic hardware capabilities generated from the Board matrix, such
as display, touch, battery, external flash, or user button. It reports fixed
presence, not live state and not raw pins, buses, DMA channels, or interrupts.

`native_layout_kind` identifies the mechanism, for example ESP partitions,
linker memory layout, or host file-backed layout. The artifact path itself is
a build detail and is not exposed through the Plugin contract.

Serial numbers, MAC addresses, radio identities, provisioning secrets, and
attestation keys identify one device rather than one Board design. They belong
to a separately permissioned Device Identity capability. Board Info therefore
remains safe to expose to ordinary local callers.

Board schema growth follows concrete hardware needs. New scalar product facts
extend `boards/config` and the generated `Board` API. New peripherals extend
the Board matrix and `boards/hal`; Board Info projects only their semantic
presence. This keeps the Plugin independent of concrete Drivers while allowing
new declarations under `boards/`.

## Plugin architecture

The two Plugins have identical lifecycle shape and independent identities:

| Plugin | Plugin ID | Provided capability | Owned Component |
| --- | --- | --- | --- |
| System Info | `system-info` | `SystemInfo` | System Info RPC Component |
| Board Info | `board-info` | `BoardInfo` | Board Info RPC Component |

Each Plugin constructor copies its source descriptor from `PluginContext`.
During `register`, it publishes one cloneable typed capability and loads one
small Component that registers the read RPC. Neither Plugin has dependencies,
persistent storage, a `start` hook, or an owner-managed task. The Component has
no runtime loop beyond its registered Event Router contract.

```text
PluginContext target metadata
       |
       +--> SystemInfoPlugin --provide--> SystemInfo
       |          `----------register--> system-info.get RPC
       |
       `--> BoardInfoPlugin ---provide--> BoardInfo
                  `----------register--> board-info.get RPC
```

The typed capabilities serve direct Plugin-to-Plugin reads. The RPCs serve
Workflow, Agent, CLI, and remote-adapter callers. Both paths return the same
semantic snapshot; serialization is confined to the RPC adapter.

The initial RPC surface contains one method per Plugin:

- `system-info.get`: empty request, one `SystemInfoSnapshot` response;
- `board-info.get`: empty request, one `BoardInfoSnapshot` response.

Reads are local, non-streaming, and side-effect free. Fixed descriptors cannot
fail after construction. System Info reads may report a clock or boot-ID error
only when the injected source explicitly supports such a failure; the default
monotonic implementation is infallible. Contract versions evolve by adding
optional response fields or a new RPC address when a semantic change is not
backward compatible.

## Placement

The implementation is split along existing ownership boundaries:

```text
platforms/api                 PlatformInfo contract
platforms/<platform>          concrete static PlatformInfo
platforms/selected            selected PlatformInfo re-export
boards/api                    enriched Board descriptor types
boards/config                 YAML validation and Rust generation
boards/configs/<board>        product identity and hardware declarations
boards/selected              selected Board descriptor re-export
composition/api               immutable target/image metadata envelope
core/plugin-api               read-only metadata in PluginContext
core/system                   boot epoch and Plugin construction
plugins/system-info           capability plus read RPC
plugins/board-info            capability plus read RPC
```

Adding a new Platform requires its descriptor in `platforms/<platform>`.
Adding a new Board requires its product and hardware declarations under
`boards/configs/<board>`. Neither addition changes either Plugin.

## Invariants

- System Info describes software, Platform, and the current boot; Board Info
  describes the selected product hardware design.
- Platform and Board identities remain independently selected and separately
  typed through composition.
- Source layers declare facts; Plugins only publish read views.
- Public models contain typed, bounded fields rather than arbitrary key/value
  metadata.
- Uptime uses a monotonic clock and never depends on wall-clock availability.
- Board Info contains no per-device identifier, secret, raw wiring, or live
  peripheral state.
- Neither Plugin imports a concrete Platform, Board, HAL, or Driver crate.
- Neither Plugin polls a background loop or mutates System state.
- New Platform- or Board-specific diagnostics use separate semantic
  capabilities instead of growing an unbounded info bag.

## Implementation sequence

1. Add and test `PlatformInfo` in `platforms/api`, then define it for every
   concrete Platform and re-export the selected value.
2. Extend the Board YAML schema and generated `Board` descriptor with product
   identity, semantic features, and native layout kind; migrate every existing
   Board bundle.
3. Add target/image metadata to composition and route the read-only values,
   plus the System boot epoch, into `PluginContext`.
4. Create `plugins/system-info` and `plugins/board-info` with capability and RPC
   tests, then run `cargo plugin sync` to update System composition.
5. Add end-to-end tests proving that changing Platform selection changes only
   Platform facts and changing Board selection changes only Board facts.
