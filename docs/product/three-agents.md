# Three agents

Barracuda puts AI in three places. Each one is chosen for the work it does best,
and each one works with Plugins as its unit of capability.

|  | On-board agent | In-browser agent | On-machine coding agents |
| --- | --- | --- | --- |
| Lives | On the device | In the Live Editor | On the developer's machine (Claude Code, Cursor, Codex) |
| Works at | Run time | Build time | Build time |
| Does | In-time coding: writes VM scripts that drive the hardware now | App development: generates device apps and their UI | Creates Plugins, Boards, Platforms; composes fully custom firmware |
| Weight | Lightweight, bounded by the device | Heavyweight, full model and full context | Heavyweight, full repository and toolchain |
| Produces | A script that runs immediately | An app that is flashed to the device | Source code that becomes firmware |
| Ladder levels | 1 | 3 | 4, 5, 6 |

## The on-board agent — in-time coding

The on-board agent runs on the device as the Agent Plugin. It owns sessions,
memory, plans, sub-agents, skills, and tools; only model inference is a remote
call to the model provider the user chose. Users reach it through any channel:
WeChat, Telegram, QQ, iMessage, the device's web chat, or the terminal.

Its signature ability is **in-time coding**. When a request touches the physical
world, the agent writes a short Lua script and runs it in the on-device VM. VM
packages expose the hardware — GPIO, I2C, SPI, UART, PWM, analog inputs,
displays, cameras, audio, IMU, LED strips, power monitoring — so the script
reads and drives real peripherals the moment it runs. Scripts can ask the user
for input mid-run, report progress, and be listed or cancelled. A script can in
turn ask the agent a question, so a running program can reason too.

For behavior that must outlive a conversation, the agent uses Workflow
automations and schedules: "every morning at seven", "when the button is
pressed", "when the temperature crosses 30°C".

**Why it is lightweight on purpose.** The device has a fixed memory budget and a
small number of execution slots. The on-board agent's job is not to build
software; it is to act — quickly, safely, and visibly — with what the device
already has. Anything larger moves to build time.

## The in-browser agent — app development

The in-browser agent lives in the Live Editor. The developer describes an app;
the agent generates its logic and its UI, and the developer refines it in a live
visual preview. When it is ready, the browser flashes it to the device over USB.

On the device, apps are managed by the App Manager Plugin and run in the app
runtime alongside every other Plugin. An app uses the device's Plugins through
the same contracts as the rest of the system, so an app can use the display, the
sensors, the network, and the on-board agent.

**Why the browser.** App generation benefits from a large model, a large context,
and a visual loop: generate, preview, adjust. The browser gives all three, with
no install, and it already holds the USB connection used for flashing.

## On-machine coding agents — Plugins, Boards, Platforms, firmware

Creating runtime code — Plugins, Boards, Platforms, and custom firmware — happens
on the developer's machine, in the Barracuda repository, with the coding agent
the developer already uses: Claude Code, Cursor, or Codex.

Barracuda ships **skills** for these agents. A skill encodes how Barracuda is
built so the agent does it the Barracuda way:

| Skill | What the agent does with it |
| --- | --- |
| `create-plugin` | Creates or updates a Plugin: chooses typed capabilities, Workflow Actions and Events, or Agent Tools; wires lifecycle and task ownership; writes schemas, docs, and focused tests |
| `create-board` | Adds a Board on an existing Platform: chip, target, flash layout, peripherals |
| `create-platform` | Brings up a new chip family behind the Platform contract without changing the System or Plugins |
| `fully-customize` | Selects or creates the Platform and Board, chooses and trims Plugins, creates missing ones, and fits the firmware to the target's resources |

**Why the machine.** This work needs the full source tree, the native toolchain,
the build, the tests, and often the hardware on the desk. That environment is
the developer's machine, and the best agents for it are the coding agents
developers already trust.

## How the three work together

- **Down the ladder:** a builder's custom firmware ships with the on-board agent;
  an app built in the Live Editor can call the on-board agent; a Plugin created
  on the machine becomes available to scripts, apps, and the Device Portal.
- **Up the ladder:** a script that proves an idea on the device becomes an app in
  the Live Editor; an app that needs a capability the runtime lacks becomes a
  Plugin; a Plugin that needs new hardware becomes a Board.
- **One contract underneath:** all three agents work through Plugin contracts —
  Agent Tools on the device, app interfaces in the editor, and the Plugin, Board,
  and Platform contracts in the repository. That shared contract is what lets
  work move between them.
