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
lists conversations, most recently used first; opening one shows its latest
messages from `AgentRuntime::session_transcript`: the person's on the right,
the Agent's on the left (an Agent message that only called Tools names
them), and a spinner while the Agent is still answering. Older messages go
off the top of the page.

## Lua API

~~~lua
local sessions = require("sessions")

sessions.list()          -- { { id, title, when = "10:31" | "昨天" | "周一" | "9月15日" }, ... }
sessions.transcript(id)  -- { { mine, text, tools = { name, ... }, open }, ... }, oldest first
sessions.agent()         -- { state = "standby" | "listening" | "thinking" | "answering", question, answer }
~~~

`transcript` returns a conversation's last 6 messages, each cut to 160
characters; `mine` marks the person's, and `open` the messages of the turn
the Agent is still answering. An unknown `id` returns `nil` and a message.

Times are UTC, like the status bar's clock. A conversation without a title
is 新对话.

## Building

`resources/app/` holds the app's sources; the `app` task builds them into
`filesystem/resources/app/` with `app-manager`'s `resources/build-app.ts`.

```sh
POCKETJS_ROOT=~/src/pocketjs cargo plugin run app --plugin builtin-app-sessions
```
