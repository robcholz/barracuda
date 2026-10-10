# The experience ladder

Barracuda is experienced as a ladder. Each level asks a little more of the
person and gives them much more freedom. Every level is useful on its own; no
one has to climb higher than their project needs. At every level an agent does
the heavy lifting.

| Level | Name | Who | What they do | Where | Agent |
| --- | --- | --- | --- | --- | --- |
| 1 | Out of the box | Everyone | Install from the browser, then ask the board to do things | Browser, phone, device | On-board agent writes and runs VM scripts |
| 2 | Configure | Makers, educators | Enable and disable Plugins to shape the device | Device Portal | — (configuration, not generation) |
| 3 | Build apps | App developers | Describe an app, generate its logic and UI, flash it | Live Editor (browser) | In-browser agent |
| 4 | Create Plugins | Plugin developers | Add new capabilities to the runtime | Developer's machine | Coding agent + `create-plugin` skill |
| 5 | Add Boards and Platforms | Contributors, board vendors | Bring Barracuda to a new board or a new chip family | Developer's machine | Coding agent + `create-board`, `create-platform` skills |
| 6 | Fully customize | Builders | Choose or create the Platform and Board, trim or create Plugins, fit the firmware to a resource budget | Developer's machine | Coding agent + `fully-customize` skill |

## Level 1 — Out of the box

**Goal:** a working, talking, acting device in minutes, with no toolchain.

1. Open the Web Installer, choose a board and a version, plug in USB, install.
2. Join Wi-Fi from the same page; the installer hands off to the Device Portal.
3. Register a model and bind a channel (WeChat, Telegram, QQ, iMessage, web chat,
   or the terminal).
4. Ask the board for something physical: "blink the LED twice a second",
   "show the temperature on the screen", "tell me when the light level drops".

The on-board agent answers by writing a short VM script and running it against
the real hardware immediately. The script, its output, and its result appear in
the conversation. Long-running behavior is kept alive by Workflow automations
and schedules, so the device keeps working after the chat ends.

**What makes it work:** the Web Installer, the Device Portal, the on-board
Agent, the VM and its peripheral packages, channels, Workflow.

## Level 2 — Configure

**Goal:** make the device fit the project without writing code.

The Device Portal lists the Plugins on the device. Users enable what they need —
camera, display, LED strip, sensors, web search, a messaging channel — and
disable what they do not, freeing memory and simplifying the device. Each Plugin
brings its own settings page, so new capabilities arrive with their own
configuration surface.

**What makes it work:** Plugin declarations, per-Plugin portal pages, the
Plugin manager.

## Level 3 — Build apps

**Goal:** a real device application with its own UI, without setting up a
firmware project.

In the Live Editor, the developer describes the app. The in-browser agent
generates the app logic and its interface, the developer previews and iterates
visually, and then flashes it to the board over USB from the same page. Apps run
on the device under the App Manager Plugin, which installs, starts, stops, and
updates them alongside the rest of the runtime.

**What makes it work:** the Live Editor, the in-browser agent, the App Manager
Plugin, the app runtime, browser flashing.

## Level 4 — Create Plugins

**Goal:** add a new capability to Barracuda that any device can use.

The developer clones the repository and asks their coding agent — Claude Code,
Cursor, or Codex — to create a Plugin. The `create-plugin` skill gives the agent
Barracuda's architecture: when to expose a typed capability, a Workflow Action or
Event, or an Agent Tool; how lifecycle and task ownership work; how the Plugin
documents itself; and which focused tests to write. The result is a Plugin that
looks like every first-party Plugin.

**What makes it work:** the Plugin contract, `plugin.toml`, agent-friendly API
conventions, the `create-plugin` skill, per-Plugin docs.

## Level 5 — Add Boards and Platforms

**Goal:** bring Barracuda to new hardware.

- **A new Board** on a supported chip is a description: chip, toolchain target,
  flash layout, and peripherals. The `create-board` skill produces it and its
  layout, and the board appears in the Web Installer.
- **A new Platform** brings a new chip family: executor entry, network stack,
  filesystem, flash, and HAL adapters behind the Platform contract. The
  `create-platform` skill guides the agent through that contract while keeping
  the System and every Plugin unchanged.

**What makes it work:** the Board YAML and Platform contract, independent Board
and Platform selection, the `create-board` and `create-platform` skills.

## Level 6 — Fully customize

**Goal:** a product firmware shaped exactly to a chip, a resource budget, and a
job.

The builder states the target and the requirements. With the `fully-customize`
skill, the coding agent selects or creates the Platform and Board, chooses the
Plugin set, trims Plugins that are too heavy for the budget, creates the ones
that are missing, and verifies that the result fits the device's memory and
flash. The output is a firmware that contains only what the product needs.

**What makes it work:** every level below it — Plugins, Boards, Platforms, and
the skills that create them — composed by one agent with one goal.

## Moving between levels

- Every level hands off to the next one with something concrete: the script the
  on-board agent wrote can become an app; an app that needs a new capability
  becomes a Plugin; a Plugin that needs new hardware becomes a Board.
- Nothing at a higher level breaks a lower one. A fully customized firmware still
  installs from the browser, still has a Device Portal, and its on-board agent
  still writes scripts.
- The docs are organized by level, so each reader starts where they are.
