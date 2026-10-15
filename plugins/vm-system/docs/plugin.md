# VM System Plugin

Gives system apps the device's settings and state as the require-only
`system` Lua package.

- Plugin ID: `vm-system`
- Dependencies: `vm`, `ui` (for the system icon table), `shell`, `app`,
  `wifi`, `agent`, `workflow`, `time`
- Settings: none
- Context resources: `target_identity`
- Provided capabilities: none
- Required capabilities: `LuaPackageRegistry` from `vm`; `Preferences`,
  `Notifications`, and `AgentStatus` from `shell`; `AppManager` from `app`;
  `WifiControl` from `wifi`; `AgentRuntime` from `agent`; `WorkflowService`
  from `workflow`; `UtcClock` from `time`
- Workflow: none
- Agent Tools: none
- Lua packages: `system`
- Web: none
- Tasks: one service task that answers `system` calls, several at once
- Storage: reads only; `PluginFilesystem::Inspect`, to add up the Workspace
  areas and the installed apps under `/plugins/media/app/apps`

The system apps (Settings, Sessions, Workflows, Files) show and change the
device through this package. They are the second layer of the device UI, see
`.agents/docs/device-ui.md`: the `app` Plugin grants a system app every Lua
package, and refuses an installable app whose manifest asks for `system`.

Each call queues a job for the service task, which owns the capabilities;
at most four calls wait at once, and a fifth fails as busy. Revoking the
package fails later calls in states that already hold it.

## Lua API

Every function returns its value, or `nil` and a message.

~~~lua
local system = require("system")

system.status()
-- { wifi = { state = "connected" | "connecting" | "disconnected", ssid = ... },
--   brightness = 70, volume = 60, muted = false, microphone = true,
--   quiet = false, screen_off = 30,
--   agent = { state = "standby" | "listening" | "thinking" | "answering",
--             question = ..., answer = ... },
--   device = { version = "0.1.0", board = ..., chip = ..., platform = ... } }

system.scan()                 -- { { ssid = ..., bars = 1..4, glyphs, secured = true }, ... }
system.set("brightness", 40)  -- also volume, muted, microphone, quiet, screen_off
system.apps()                 -- { { id, name, version, icon, glyphs, letter, system, state }, ... }
system.sessions()             -- { { id, title, when = "10:31" | "昨天" | "周一" }, ... }
system.workflows()            -- { { id, event, trigger }, ... }
system.storage()              -- { used, label, parts = { { name, icon, bytes, label }, ... } }
system.files()                -- { { name = "媒体", count, present }, ... }
system.files("媒体")          -- { name, present, count, label, files = { { path, label }, ... } }
system.notify(title, body, icon)
system.ask(title, body)       -- waits: { allowed = true, answered = true }
system.agent("thinking", question, answer)
~~~

`set` returns the new `status()`. Brightness and volume are percents,
`screen_off` is one of the shell's `SCREEN_TIMEOUTS` in seconds (0 keeps the
screen on). The shell applies brightness and the screen timeout; the volume is
kept for audio output to read.

`glyphs` are an icon's layers in the system icon font, `{ base, accent,
dim }`, each a one-character string or `""`; a view stacks them tinted
`#FAFAFA`, `#CCFF00`, and `#52525B`. `state` is `running`, `background`, `suspended`, `terminating`, or absent.
Session times are UTC, like the status bar's clock. `files` lists up to 64
files of an area, sorted by path.
