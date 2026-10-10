# Vision

## In one sentence

Barracuda is a plugin-based embedded runtime and agentic platform: Plugins are
the primitive every device is built from, and AI agents do the building at every
level — from writing a script that drives the hardware right now to producing a
firmware tailored to a new board.

## The problem

Embedded development is the last corner of software where the modern
development experience has not arrived.

- **Getting started is a toolchain problem.** Before a board does anything
  interesting, a newcomer installs compilers, drivers, SDKs, and flashing tools,
  and fights each of them. Many people stop here.
- **Every project starts from scratch.** Wi-Fi provisioning, a web UI, storage,
  scheduling, messaging, and an LLM client are rebuilt for each product, on each
  chip, with each vendor's SDK.
- **AI stops at the edge of the device.** Agents can now write most of a web
  application, but on a microcontroller they mostly produce snippets that a human
  still has to wire into a board support package, a build system, and a memory
  budget. And when a device does "have AI", it is usually a thin client to a
  cloud service; the device itself cannot reason or adapt.
- **Hardware is fragmented.** Each chip family has its own SDK, executor,
  network stack, and flashing story. Moving a project to a different board often
  means rewriting it.

## What Barracuda is

**A runtime made of Plugins.** A Barracuda device runs one portable System that
registers a set of Plugins and starts them together. Wi-Fi, the web server, the
device portal, the Agent, the scripting VM, Workflow automation, every message
channel, and every peripheral binding is a Plugin. The same System runs on
microcontrollers and on desktop hosts; only the Platform underneath changes.

**An agent on the device.** Every Barracuda device carries a lightweight agent.
It runs on the device itself, talks to the user through the channels they already
use, and does in-time coding: it writes a short VM script, runs it against the
real hardware, and reports back — in the same conversation.

**Agents at build time.** Building *for* the device is agentic too. An
in-browser agent builds applications and their UI in the Live Editor. On the
developer's machine, coding agents such as Claude Code, Cursor, and Codex use
Barracuda's skills to create Plugins, add Boards and Platforms, and compose fully
custom firmware.

**A ladder, not a cliff.** People start by installing from a browser and talking
to their board. They can stop there, or climb: switch Plugins on and off, build
an app, write a Plugin, add a Board, and finally shape the whole firmware to a
chip and a budget. Each step up trades a little ease for a lot of freedom, and
each step has an agent doing the heavy lifting.

## Principles

1. **Plugins are the primitive.** Every capability is a Plugin with a declared
   contract. Customizing a device means choosing, configuring, trimming, or
   writing Plugins — never forking the runtime.
2. **Agentic at every level.** If a step can be done by an agent, the product is
   designed so that an agent can do it well: agent-friendly contracts, skills
   that encode our conventions, documentation written to be read by models as
   much as by people.
3. **The right agent in the right place.** In-time work happens on the device
   with a small agent and a small script. Heavy generation happens where compute
   and context are plentiful: in the browser and on the developer's machine. We
   never ask the device to do the work of a workstation.
4. **Modern development experience.** No toolchain to start. Install from a
   browser, configure from a web page, chat from a phone. When developers do open
   the source, it is one Cargo workspace, one command to select a board, one
   command to run.
5. **Same System, every target.** A Board and a Platform are selected
   independently. The portable System and its Plugins do not change between an
   ESP32, an STM32, and a laptop, so work moves between targets instead of being
   rewritten.
6. **Fit the resources.** Barracuda scales down to a microcontroller with a few
   hundred kilobytes of RAM and up to boards with external memory and desktops.
   The firmware a builder ships contains only the Plugins it needs.
7. **Open and inspectable.** Users see what the agent wrote and what it ran.
   Developers read the same source, docs, and skills the agents read.

## What success looks like

- A student plugs in a board, opens a web page, and within minutes asks the
  board to blink an LED in a pattern — and watches it happen, with the script
  that did it on screen.
- A maker turns on the camera and display Plugins, turns off what they do not
  need, and has a device that fits their project without touching code.
- An app developer describes a small device app in the Live Editor, previews its
  UI, and flashes it.
- A Plugin author asks their coding agent to "create a plugin for this sensor"
  and gets a Plugin that follows every Barracuda convention, with tests and docs.
- A board vendor adds their board with a skill, and it appears in the Web
  Installer for their customers.
- A builder ships a product firmware sized exactly to its chip and its job.
