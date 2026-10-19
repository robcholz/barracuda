<a href="https://barracuda-os.vercel.app">
  <img width="1024" alt="Barracuda" src=".github/banner.jpg" />
</a>

<p align="center">
  <a href="https://barracuda-os.vercel.app">Website</a>
  ·
  <a href="https://barracuda-os.mintlify.site">Docs</a>
  ·
  <a href="https://barracuda-os.mintlify.site/get-started/quickstart">Quickstart</a>
  ·
  <a href="https://barracuda-os.vercel.app/boards/">Boards</a>
  ·
  <a href="https://barracuda-os.mintlify.site/product/plugins">Plugins</a>
  ·
  <a href="https://github.com/robcholz/barracuda/releases">Releases</a>
  ·
  <a href="https://barracuda-os.mintlify.site/zh">中文</a>
</p>

<h1></h1>

## About

Barracuda is the agentic platform for embedded devices: everything is a Plugin,
and an agent helps at every step. On the device, the on-board Agent writes Lua
scripts that drive the hardware. On your computer, your coding agent writes
Plugins, Boards, and firmware.

Every capability on a device is a Plugin, composed into one firmware for a
selected Board. The same System runs on ESP32 and STM32 microcontrollers and on
Linux and macOS, so you can try it without any hardware.

## Quickstart

Install [rustup](https://rustup.rs), then run Barracuda on your computer with
virtual peripherals:

```bash
git clone https://github.com/robcholz/barracuda
cd barracuda
cargo board select local-linux   # or local-macos
sudo sh platforms/linux/provision.sh barracuda0   # Linux only, once per boot
BARRACUDA_VIRTUAL_IO_ADDR=127.0.0.1:7878 cargo run
```

Open [http://127.0.0.1:7878/](http://127.0.0.1:7878/) to see the virtual
display. In a second terminal, connect a model and talk to the Agent:

```bash
cargo cli configure   # choose Local, then the model configuration (agent)
cargo cli             # choose Local, then ask: "Set pin vio-0 high"
```

The Agent writes a short Lua script, runs it against the pin, and replies with
the result. Read the [Quickstart](https://barracuda-os.mintlify.site/get-started/quickstart)
for each step.

## Run it on a board

Select a Board and run it. On a device Board, `cargo run` builds the firmware,
flashes it, and keeps the logs in your terminal:

```bash
cargo board select   # searchable list; prints the toolchain install command if one is missing
cargo run
```

Barracuda supports more than 70 Boards from Espressif, M5Stack, ST, and
Unexpected Maker, across ESP32, ESP32-S3, ESP32-C3, ESP32-C6, ESP32-P4, and
STM32U5. See the [Board list](https://barracuda-os.vercel.app/boards/) and
[Installation](https://barracuda-os.mintlify.site/get-started/installation)
for the toolchain each one needs.

## The ladder

Each level is useful on its own, and each one gives you more freedom than the
one below it. The docs follow the same levels.

| Level | What you do | Who helps |
| --- | --- | --- |
| [01 · Use your device](https://barracuda-os.mintlify.site/use/device-portal) | Ask the board to do things | The on-board Agent writes and runs scripts |
| [02 · Configure](https://barracuda-os.mintlify.site/boards/profiles) | Choose the Plugins in the firmware, build, flash | A profile, a Board file, or your coding agent |
| [03 · Apps](https://barracuda-os.mintlify.site/architecture/device-ui) | Build apps for the device's screen | The in-browser agent |
| [04 · Plugins](https://barracuda-os.mintlify.site/develop/write-a-plugin) | Add a capability to the runtime | Your coding agent with `create-plugin` |
| [05 · Boards and Platforms](https://barracuda-os.mintlify.site/boards/overview) | Bring up a new board or chip family | Your coding agent |
| [06 · Custom firmware](https://barracuda-os.mintlify.site/develop/custom-firmware) | Fit a firmware to a chip and a budget | Your coding agent |

## How it is built

Four layers make up the repository.

| Layer | Owns | Directory |
| --- | --- | --- |
| Board | The chip, toolchain target, flash layout, peripherals, and Plugin profile | [`boards/`](boards/) |
| Platform | Executor entry, network stack, filesystem, flash, and HAL adapters | [`platforms/`](platforms/) |
| System | Registers every Plugin in dependency order, then starts them | [`core/system/`](core/system/) |
| Plugins | Capabilities, Workflow Actions and Events, Agent Tools, portal pages, tasks | [`plugins/`](plugins/) |

## Contributing

Barracuda is built to be worked on with a coding agent. The repository ships
skills for Claude Code in [`.claude/skills/`](.claude/skills/), such as
`create-plugin`, `create-board`, and `create-platform`, and the conventions
every agent follows in [`AGENTS.md`](AGENTS.md).

- Report bugs and request features in
  [Issues](https://github.com/robcholz/barracuda/issues).
- Start with [Writing docs](docs/contributing/docs.mdx) and the
  [Plugin API](docs/architecture/plugin-api.mdx) before your first pull
  request.

## Documentation

All documentation lives in [`docs/`](docs/) and is published at
[barracuda-os.mintlify.site](https://barracuda-os.mintlify.site), in English and
Simplified Chinese. Preview it locally from `docs/`:

```bash
npx mint dev
```
