# Builtin App Sessions Plugin

Ships 会话 (Sessions), a system app that lists the Agent's conversations.

- Plugin ID: `builtin-app-sessions`
- Dependencies: `app-manager`, `agent`, `time`
- Settings: none
- Context resources: none
- Provided capabilities: none
- Required capabilities: `AppManager` and `AgentStatus` from `app-manager`,
  `AgentRuntime` from `agent`, `UtcClock` from `time`
- Workflow: none
- Agent Tools: none
- Lua packages: `sessions`, for this app only
- Web: none
- Tasks: none; `app-manager`'s task answers the package
- Storage: the app's container in `/resources/app`

The app is a builtin app, see the `app-manager` Plugin's
[`plugin.md`](../../app-manager/docs/plugin.md#builtin-apps). Its first page
lists conversations, most recently used first; opening one shows what was
first asked in it and the Agent's latest words on this device. The Agent
runtime keeps no transcript a view can read, so a conversation's full history
is not shown.

## Lua API

~~~lua
local sessions = require("sessions")

sessions.list()   -- { { id, title, when = "10:31" | "昨天" | "周一" | "9月15日" }, ... }
sessions.agent()  -- { state = "standby" | "listening" | "thinking" | "answering", question, answer }
~~~

Times are UTC, like the status bar's clock. A conversation without a title
is 新对话.

## Building

`resources/app/` holds the app's sources; the `app` task builds them into
`filesystem/resources/app/` with `app-manager`'s `resources/build-app.ts`.

```sh
POCKETJS_ROOT=~/src/pocketjs cargo plugin run app --plugin builtin-app-sessions
```
