# App Manager Plugin

- Plugin ID: `app-manager`
- Direct dependencies: `vm`, `ui`, `workflow`
- Provided typed capabilities: `AppManager`
- Required typed capabilities: `Vm` from `vm`, `UiService` from `ui`,
  `WorkflowActionRegistry` from `workflow`
- Owned Components: none
- Plugin-owned tasks: one task that supervises the foreground app's VM run
- Workflow Actions: `app.list`, `app.launch`, `app.stop`, `app.status`, `app.tap`
- Events, Agent Tools: none
- Storage: private Plugin filesystem; reads bundles and writes nothing. Retains
  its Workflow Action registrations for its lifetime.

## Apps

An app is a Lua program with an optional compiled MicroTS view. Apps without
a view run headless. A bundle is a directory named after the app ID, which is
1–32 characters of `a-z`, `0-9`, and `-`:

```text
<id>/app.json       {"id": "<id>", "name": "…", "version": "…"}
<id>/main.lua       the program, at most 64 KiB
<id>/view.bvir      optional: the linked view
<id>/styles.bin     its stylesheet
<id>/font-<n>.bin   its baked fonts; view, styles, and fonts total at most 512 KiB
```

App containers are carved out of this Plugin's own private filesystem; the
Plugin's scope is unchanged and no shared Workspace path is used. Bundles ship
in the image under `/resources/apps` (this Plugin's
`filesystem/resources/apps`) or are installed under `/media/apps`. An installed
bundle shadows a bundled one with the same ID. A bundle whose
manifest ID differs from its directory name is skipped.

## Running

`AppManager::launch` stops the foreground app, opens the new app's view with
`UiService::open`, and starts `main.lua` on the VM with a global prelude on
its first line:

```lua
APP = { id = "counter", ui = "view-7" }  -- ui is nil for a headless app
```

The program claims its view with `require("ui").attach(APP.ui)` and usually
loops on `view:next()`. One app is in the foreground at a time and it holds
one of the VM's four execution slots. The Plugin task watches the run: when
the program ends, its view closes and `AppManager::last_exit` records the
outcome, diagnostic, and last output lines. Apps have no input; a read gets
EOF. `AppManager::stop` closes the view and cancels the run without recording
an exit.

No display backend is connected yet. `AppManager::tap` and
`AppManager::texts` stand in for touch input and rendering, and views are laid
out at the `ui` Plugin's default viewport.

## Building bundles

App sources live in `resources/apps/<id>/`: `app.json`, `main.lua`, and an
optional `app.tsx` view with its `app.d.ts` contract and `pocket.json`.
`resources/apps/build.ts` compiles them into `filesystem/resources/apps`. It
needs a PocketJS checkout for the MicroTS compiler and font baker, so it is a
manual task and its outputs are committed:

```sh
POCKETJS_ROOT=~/src/pocketjs cargo plugin run apps --plugin app-manager
```
