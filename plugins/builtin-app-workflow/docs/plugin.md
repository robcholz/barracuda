# Builtin App Workflow Plugin

Ships 工作流 (Workflows), a system app that lists the loaded Workflows and
what starts each.

- Plugin ID: `builtin-app-workflow`
- Dependencies: `app-manager`, `workflow`
- Settings: none
- Context resources: none
- Provided capabilities: none
- Required capabilities: `AppManager` from `app-manager`, `WorkflowService`
  from `workflow`
- Workflow: none
- Agent Tools: none
- Lua packages: `workflows`, for this app only
- Web: none
- Tasks: none; `app-manager`'s task answers the package
- Storage: the app's container in `/resources/app`

The app is a builtin app, see the `app-manager` Plugin's
[`plugin.md`](../../app-manager/docs/plugin.md#builtin-apps). A loaded
Workflow shows as on; the Workflow runtime has no switch to turn one off, so
the switches only show the state.

## Lua API

~~~lua
local workflows = require("workflows")

workflows.list()  -- { { id, event, trigger = "定时" | "网络变化时" | ... }, ... }
~~~

`event` is the Workflow's Event rule and `trigger` says it in words.

## Building

`resources/app/` holds the app's sources; the `app` task builds them into
`filesystem/resources/app/` with `app-manager`'s `resources/build-app.ts`.

```sh
POCKETJS_ROOT=~/src/pocketjs cargo plugin run app --plugin builtin-app-workflow
```
