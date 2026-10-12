# Peripheral Instances and Plugin Claims

Status: **proposed**. Once accepted, it lands as one Plugin API change set
(core, every affected Plugin, [`plugin-api.md`](plugin-api.md),
[`platform-architecture.md`](platform-architecture.md), the `create-plugin`
skill, and the `cargo plugin sync --check` rules). Until then the current
documents remain authoritative.

## Problem

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

## Model

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

Then:

- a device taken by two slots is an error, because peripherals are move-only;
- a required slot left empty is an error;
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
| One display (most Boards today) | that display | nothing (empty list) |
| `front` (primary), `back-eink`, `status` | `front` | `back-eink`, `status` |
| One display, `ui` disabled | — | that display, as today |

The last row is why `"*"` collects an unclaimed primary device too: products
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

A required slot's constant always names a device, so its `take` cannot be
`None` in a build that passed `cargo plugin sync --check`; the Plugin still
handles `None` instead of panicking. A `"*"` slot is a constant list of
`(name, instance)` pairs; the Lua `display` package then offers
`display.list()` and `display.open(name)`, and `display.open()` without a name
keeps opening the first one.

## Migration

1. Peripheral APIs, the generator, and `board.yml` validation gain named
   instances, the `primary` mark, and per-kind enums. Boards with one device
   per kind need no edits.
2. `plugin.toml` gains `[peripherals]`; `cargo plugin sync --check` resolves
   and validates claims; the Plugin build emits claim constants.
3. Every Plugin that calls `take_<peripheral>()` declares its slots and takes
   by constant, in the same change set. The `display` Plugin declares
   `rest = "*"` and its Lua package gains named displays.
4. `plugin-api.md` (construction, `plugin.toml`, the one-way table) and
   `platform-architecture.md` (Board resources, peripherals, review checklist)
   absorb this document, which is then removed.
