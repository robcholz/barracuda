# Product surfaces

Barracuda is one product experienced through several surfaces. Each has one job
and hands off to the next.

```text
Website ──► Web Installer ──► Device Portal ──► Channels
   │              │                 │
   │              └──► Live Editor (build apps, flash)
   │
   └──► Docs ──► Repository + skills (coding agents on the developer's machine)
```

## Website

**Job:** explain what Barracuda is in a minute and send each visitor to the
right first step.

The website speaks in outcomes, not internals. It introduces the plugin-based
runtime, the agent on every device, and the ladder from out-of-the-box to fully
custom firmware. It routes people by intent: install a board, build an app, read
the docs, partner as a board vendor, or bring Barracuda to a classroom.

## Web Installer

**Job:** take a board from blank to online in minutes, from a browser.

1. Choose a board and a version.
2. Connect over USB; the installer identifies the chip and any existing install.
3. Install, with clear progress and recovery.
4. Join Wi-Fi over the same USB connection.
5. Hand off to the Device Portal.

The Web Installer is also the first step of the in-browser toolset: the same
connection later flashes apps built in the Live Editor.

## Device Portal

**Job:** configure and understand the device, from any browser on the local
network.

The portal is served by the device. It shows the device's state and the Plugins
it runs. Each Plugin contributes its own page: Wi-Fi, models, web search, each
channel, files, and anything a new Plugin adds. Users configure each Plugin
here, and the device's web chat lives here too.

## Channels

**Job:** reach the on-board agent wherever the user already is.

WeChat, Telegram, QQ, iMessage, Inkbox, the device's web chat, and the terminal
are all channels to the same agent, with the same sessions. Each channel is a
Plugin.

## Live Editor

**Job:** build device apps with an agent, visually, and flash them.

The developer describes an app; the in-browser agent generates its logic and UI;
the developer previews and iterates live; the browser flashes the result. Apps
are managed on the device by the App Manager Plugin and use the device's Plugins
through the same contracts as the rest of the system.

## Skills

**Job:** let coding agents build Barracuda the Barracuda way.

Skills live in the repository and are read by Claude Code, Cursor, and Codex.
They cover creating Plugins, Boards, and Platforms, and composing fully custom
firmware. Skills are the primary interface for runtime development; the
conventions they encode are the same ones human contributors follow.

## CLI

**Job:** a fast loop for developers.

`cargo board select` chooses the Board; `cargo run` builds, flashes, and runs;
`cargo image deploy` writes Plugin resources; `cargo plugin run build` builds
Plugin pages; `cargo cli` connects a terminal channel to a running device.

## Docs

**Job:** the reference for every level of the ladder, readable by people and by
agents.

The docs are organized by level — out of the box, configure, build apps, create
Plugins, Boards and Platforms, fully customize — with a reference section for
every Plugin, command, and file format. Every page can be copied as Markdown or
opened in an AI assistant, and the docs are served to coding agents as context.
