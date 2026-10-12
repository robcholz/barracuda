# Per-Board Plugins and Peripheral Claims

Status: **proposed**. Once accepted, it lands as one Plugin API change set
(core, every affected Plugin, [`plugin-api.md`](plugin-api.md),
[`platform-architecture.md`](platform-architecture.md), the `create-plugin`
skill, and the `cargo plugin sync --check` rules). Until then the current
documents remain authoritative.

## Problem

- **Plugins are enabled globally.** The enabled set lives in the ignored
  `.barracuda/disabled-plugins` and is the same for every Board. A Board cannot
  say that it is a touch-screen device running the system UI while another is
  a headless agent gateway, and CI builds every Board with one Plugin set.
- **One instance per kind.** Each peripheral API exposes a single primary
  instance (`DisplayPeripheral::take_display() -> Option<Display>`), and the
  generator rejects a Board that declares a second device of the same kind
  (`DuplicatePeripheral`). A product with a main touch screen and a small
  e-paper status panel cannot be described.
- **First constructor wins.** A Plugin takes its peripheral in `new` with
  `context.hal.peripherals.take_<peripheral>()`. When two enabled Plugins want
  the same kind, the one constructed first gets it and the other silently gets
  `None`; registration order, not intent, decides.
- **Boards are open-ended.** There are many Boards and there will be custom
  ones. A Plugin author cannot know what each Board calls its devices, and a
  Board author should not edit Plugins to fit a product.

## Plugin enablement per Board

Which Plugins a firmware contains is a property of the Board it is built for,
committed with the Board. Plugins fall into three groups, decided by
different facts:

| Group | Examples | Enabled when |
| --- | --- | --- |
| Infrastructure | `workflow`, `vm`, `http`, `webserver`, `wifi`, `time` | almost always |
| Hardware adapters | `display`, `vm-gpio`, `vm-i2c`, `camera`, `audio` | the Board has the hardware |
| Product features | `ui`, `app-manager`, `agent`, `imessage-*` | the product built on the Board wants them |

The third group is a product decision, not a hardware fact: the same Board can
ship as a touch device or as a headless gateway. Named profiles capture those
decisions once.

### Profiles

A profile is a named Plugin set under `boards/profiles/<name>.yml`, optionally
extending one other profile:

```yaml
# boards/profiles/base.yml
enable: [workflow, vm, http, webserver, wifi, time, captive-portal]
```

```yaml
# boards/profiles/device.yml
extends: base
enable: [ui, app-manager, display]
```

### Board selection of Plugins

A Board names one profile and overrides individual Plugins in its existing
`plugins.<id>` section, next to their settings:

```yaml
profile: device
plugins:
  imessage-gateway:
    enabled: false
  agent:
    enabled: true
    # settings stay here as before
```

A Board without `profile` uses `base`.

### The enabled set

`cargo board select` and `cargo plugin sync --check` compute each Board's
enabled set:

1. Start from the profile chain: each profile's `enable` on top of the one it
   extends.
2. Apply the Board's `plugins.<id>.enabled` overrides.
3. Add every Plugin an enabled Plugin depends on, transitively. A Board
   enables features; their dependencies follow. A dependency the Board
   explicitly disables is an error naming the dependent.
4. Apply the local `.barracuda/disabled-plugins` override, which remains a
   developer convenience and is never committed. A locally disabled Plugin
   disables its dependents too, and sync prints why.

An enabled Plugin whose required peripheral claim cannot be met on the Board is
an error (see Resolution): enabling it was explicit, so the Board cannot
deliver it. A Plugin that is not enabled makes no claims.

Profiles and `plugins.<id>` are the only hand-written files that name Plugins
outside the Plugins themselves; `cargo plugin sync --check` validates that
every name they use is a Plugin.

### Build composition

Today `cargo plugin sync` writes one System registration list for the global
enabled set. With per-Board sets, the generated registration list, workspace
dependencies, and image selection cover every Plugin, each behind a feature of
System, and Board selection activates the features of the Board's enabled set,
the same way it already activates only the peripheral implementations the
Board uses. Switching Boards therefore changes no committed file, and CI
builds every Board with the set it ships.

## Peripheral claims

Three parties describe three things, each in the place it already owns.

| Party | File | Describes | Never names |
| --- | --- | --- | --- |
| Plugin author | `plugins/<id>/plugin.toml` | **Needs**: which peripherals the Plugin uses, under names local to the Plugin, and whether each is optional | a Board or a device name |
| Board author | `board.yml` → `peripherals.devices` | **Supply**: the devices on the Board, under the Board's own names, and which one is primary | a Plugin |
| Board author | `board.yml` → `plugins.<id>` | **Wiring**, only when it cannot be derived | hardware details |

`plugins.<id>` is the existing per-Board Plugin settings section and the only
place a Board names a Plugin, so the layers below the Plugins stay
Plugin-neutral.

### Needs: `[peripherals]` in `plugin.toml`

A Plugin declares one table per peripheral kind. Keys are slot names chosen by
the Plugin; values say what fills the slot.

```toml
# plugins/ui/plugin.toml
[peripherals.display]
main = "primary"            # the Board's primary display

[peripherals.touch]
main = "primary"
```

```toml
# plugins/display/plugin.toml (the Lua display package)
[peripherals.display]
rest = "*"                  # every display no other Plugin claims
```

| Value | Fills the slot with |
| --- | --- |
| `"primary"` | the device the Board marks primary for that kind |
| `"any"` | exactly one device of that kind, chosen by the rules below |
| `"*"` | every remaining device of that kind, as a list |
| `{ fill = "...", optional = true }` | the same, but the Plugin starts without it |

A Plugin may declare several slots of one kind, so one Plugin can drive two
displays or three LED strips. Kinds are the peripheral API module names:
`display`, `touch`, `buttons`, `indicator`, `led-strip`, `camera`,
`audio-codec`, `imu`, `power-monitor`, `real-time-clock`. Removable storage
stays System-owned, because System runs its lifecycle task.

### Supply: named devices in `board.yml`

Devices are already named under `peripherals.devices`. A Board with several
devices of one kind marks one of them primary:

```yaml
peripherals:
  devices:
    front:
      implementation: mipi-dbi-display
      primary: true
      bindings: { ... }
    back-eink:
      implementation: ssd1680-display
      bindings: { ... }
    front-touch:
      implementation: cst816-touch
      bindings: { ... }
```

A kind with one device needs no mark; that device is primary. Several devices
of a kind without exactly one `primary: true` is a Board validation error.
`primary` is a hardware fact (the product's main screen), not a Plugin choice.

### Wiring: `plugins.<id>.peripherals` in `board.yml`

Wiring is written only to resolve an ambiguity or to override a default:

```yaml
plugins:
  ui:
    peripherals:
      display.main: back-eink   # this product runs the system UI on the e-paper
```

## Resolution

`cargo board select` and `cargo plugin sync` resolve every slot of every
enabled Plugin against the selected Board, in this order:

1. **Wired.** A slot named in `plugins.<id>.peripherals` takes that device.
2. **Primary.** A `"primary"` slot takes the Board's primary device of its kind.
3. **Any.** An `"any"` slot takes the only device of its kind not yet taken.
4. **Rest.** A `"*"` slot takes every device of its kind still untaken, in
   Board order. At most one enabled Plugin may declare `"*"` per kind.

Only enabled Plugins take part. Then:

- a device taken by two slots is an error, because peripherals are move-only;
- a required slot left empty is an error: the Board enabled a Plugin it cannot
  serve;
- an optional slot left empty is delivered as `None`, and an empty `"*"` slot
  as an empty list;
- an `"any"` slot with several candidates is an error;
- wiring that names an unknown device, a device of another kind, or a slot the
  Plugin does not declare is an error, checked like any other Plugin setting.

Every error names the slots and devices involved and prints the
`plugins.<id>.peripherals` lines that would resolve it.

The result is a per-Plugin claim table fixed at build time. The firmware never
compares names.

### Examples

| Board | `ui` gets | `display` (Lua) gets |
| --- | --- | --- |
| `device` profile, one display | that display | nothing (empty list) |
| `device` profile, `front` (primary), `back-eink`, `status` | `front` | `back-eink`, `status` |
| `base` profile plus `display`, one display | — (not enabled) | that display, as today |
| `device` profile, no display | build error: `ui` needs a primary display | — |

The third row is why `"*"` collects an unclaimed primary device too: products
without a system UI keep using the main screen from Lua. With both Plugins
enabled, the system UI owns the primary screen and Lua drives every other
screen, with no wiring on any Board.

## Generated interfaces

The peripheral APIs move from one instance per kind to claimed instances,
while dispatch stays static:

- For each kind the generator emits one Board-specific enum over the
  implementations the Board uses, implementing the peripheral trait by
  matching, for example
  `enum BoardDisplay { Front(MipiDbiDisplay), BackEink(Ssd1680Display) }`.
  A Board with one device of a kind has a single-variant enum; there is no
  `dyn`, and the match compiles away.
- `GeneratedPeripherals` keeps one `Option` per device, as today, and gains
  indexed access per kind:
  `take_display(DisplayInstance) -> Option<BoardDisplay>`.
- A Plugin's `build.rs` already resolves its settings against the selected
  Board. It also emits the Plugin's claim table as constants, so a slot is a
  typed instance handle, not a string:

```rust,ignore
mod config {
    barracuda_plugin::config!();   // also defines config::peripherals::*
}

impl UiPlugin {
    pub fn new<Builtins, Io>(context: &mut PluginContext<Builtins, Io>) -> Self {
        let peripherals = &mut context.hal.peripherals;
        Self {
            display: peripherals.take_display(config::peripherals::DISPLAY_MAIN),
            touch: peripherals.take_touch(config::peripherals::TOUCH_MAIN),
        }
    }
}
```

A required slot's constant always names a device in a build that passed
resolution, so its `take` yields the device; the Plugin still handles `None`
instead of panicking. A `"*"` slot is a constant list of
`(name, instance)` pairs; the Lua `display` package then offers
`display.list()` and `display.open(name)`, and `display.open()` without a name
keeps opening the first one.

## Migration

1. Board profiles and per-Board enablement: `boards/profiles/`, `profile` and
   `plugins.<id>.enabled` in `board.yml`, the enabled-set computation, and the
   feature-gated System registration activated by Board selection. Existing
   Boards start on `base` plus the overrides that reproduce today's builds.
2. Peripheral APIs, the generator, and `board.yml` validation gain named
   instances, the `primary` mark, and per-kind enums. Boards with one device
   per kind need no edits.
3. `plugin.toml` gains `[peripherals]`; `cargo plugin sync --check` resolves
   and validates claims for every Board; the Plugin build emits claim
   constants.
4. Every Plugin that calls `take_<peripheral>()` declares its slots and takes
   by constant, in the same change set. The `display` Plugin declares
   `rest = "*"` and its Lua package gains named displays.
5. `plugin-api.md` (construction, `plugin.toml`, settings, the one-way table,
   scope) and `platform-architecture.md` (Board, Board resources, peripherals,
   review checklist) absorb this document, which is then removed.
