<a href="https://barracuda-os.vercel.app">
    <img width="1024" alt="Barracuda, the agentic platform for embedded devices" src=".github/banner.jpg" />
</a>
&nbsp;

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
  <a href="https://barracuda-os.mintlify.site/product/three-agents">Three Agents</a>
  ·
  <a href="https://barracuda-os.mintlify.site/zh">中文</a>
</p>

> [!NOTE]
> The browser installer is in development. Today you build and flash Barracuda
> from this repository with Cargo, or run it on Linux and macOS with no board
> at all.

<h1></h1>

## About

[Barracuda](https://barracuda-os.vercel.app) is the agentic platform for
embedded devices: everything is a Plugin, and an agent helps at every step.
Talk to your board and its on-board agent writes the code and runs it, or build
Plugins, Boards, and firmware with the coding agent you already use (Claude
Code, Cursor, Codex, and others).

## Installation

Read [Installation](https://barracuda-os.mintlify.site/get-started/installation)
for the toolchain your board needs, then follow the
[Quickstart](https://barracuda-os.mintlify.site/get-started/quickstart) to run
Barracuda on your computer and talk to it in minutes.

## An agent on every device

Every Barracuda device carries a lightweight agent that runs on the device
itself. Ask it for something physical, such as "blink the LED three times", and
it writes a short Lua script, runs it against the real hardware, and replies in
the same conversation. With it you can:

- Drive GPIO, I2C, SPI, UART, PWM, displays, cameras, audio, IMUs, and LED
  strips from a sentence, and see the script that did it
- Reach the same agent from WeChat, Telegram, QQ, iMessage, the device's web
  chat, or your terminal
- Turn a request into an automation that outlives the conversation: "every
  morning at seven", "when the button is pressed"
- Bring your own model: any OpenAI- or Anthropic-compatible service, configured
  from the [Device Portal](https://barracuda-os.mintlify.site/use/device-portal)

## Everything is a Plugin

Wi-Fi, the web server, the Device Portal, the agent, the scripting VM,
automations, every message channel, and every peripheral binding is a
[Plugin](https://barracuda-os.mintlify.site/product/plugins). A device runs one
portable System that starts the Plugins you choose, and a Plugin you leave out
is not in the firmware.

The same System runs on ESP32 and STM32 microcontrollers and on Linux and macOS.
More than 70 [Boards](https://barracuda-os.vercel.app/boards/) from Espressif,
M5Stack, ST, and Unexpected Maker are supported today, across ESP32, ESP32-S3,
ESP32-C3, ESP32-C6, ESP32-P4, and STM32U5.

## Build with your coding agent

This repository is built to be worked on by coding agents. It ships skills that
teach Claude Code, Cursor, and Codex how Barracuda is built, so your agent does
it the Barracuda way:

- [`create-plugin`](.agents/skills/create-plugin/SKILL.md) adds a capability:
  typed APIs, Workflow Actions and Events, Agent Tools, settings, docs, and
  tests
- [`create-board`](.agents/skills/create-board/SKILL.md) brings a new board up
  on an existing Platform
- [`create-platform`](.agents/skills/create-platform/SKILL.md) brings up a new
  chip family without changing the System or any Plugin

Read [The ladder](https://barracuda-os.mintlify.site/product/experience-ladder)
for how far each level takes you, from talking to your board to a firmware
fitted to a chip and a budget.

## Open Source & Contributing

Barracuda is open source and lives in this repository. We welcome community
contributions: new Plugins, new Boards, fixes, and docs.

### Issue to PR

Before filing, [search existing issues](https://github.com/robcholz/barracuda/issues)
for your bug or feature request. If nothing exists,
[file an issue](https://github.com/robcholz/barracuda/issues/new) with the Board
you use and the steps to reproduce.

### Building the Repo Locally

To build and run Barracuda from source on your computer:

```bash
cargo board select local-linux                      # or local-macos, or any Board under boards/configs/
sudo sh platforms/linux/provision.sh barracuda0     # Linux only, once per boot
BARRACUDA_VIRTUAL_IO_ADDR=127.0.0.1:7878 cargo run  # build and run
cargo fmt --all --check                             # formatting, as CI checks it
```

See [AGENTS.md](AGENTS.md) for the engineering guide, including the Plugin
contract, the Platform architecture, code style, and how to write docs.

## Support and Questions

1. See our [docs](https://barracuda-os.mintlify.site) for a guide to every
   level, in English and [Simplified Chinese](https://barracuda-os.mintlify.site/zh).
2. Ask questions and report problems in
   [Issues](https://github.com/robcholz/barracuda/issues).
3. Follow [Releases](https://github.com/robcholz/barracuda/releases) for what
   changed.

## Open Source Dependencies

We'd like to call out a few of the open source projects Barracuda is built on:

- [Embassy](https://github.com/embassy-rs/embassy)
- [esp-hal](https://github.com/esp-rs/esp-hal)
- [Lua](https://www.lua.org)
- [picoserve](https://github.com/sammhicks/picoserve)
- [reqwless](https://github.com/drogue-iot/reqwless)
- [embedded-graphics](https://github.com/embedded-graphics/embedded-graphics)
- [heapless](https://github.com/rust-embedded/heapless)
- [Tokio](https://github.com/tokio-rs/tokio)
