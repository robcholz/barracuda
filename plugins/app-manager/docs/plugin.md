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

An app is a Lua program with an optional compiled MicroTS view, stored as an
app container. [`app-format.md`](app-format.md) defines the container and
manifest; `schemas/app/manifest.json` checks the manifest.

App containers are carved out of this Plugin's own filesystem scope, which is
unchanged: installed apps under the private `/media/apps`, bundled apps under
the private `/resources/apps`, and apps that Agents create under the shared
`/workspace/media/apps`, which this Plugin only reads. An installed app
replaces a bundled one with the same ID; a Workspace container never replaces
either.

## Running

`AppManager::launch` stops the foreground app, opens the new app's view with
`UiService::open`, and starts its entry program on the VM with a global
prelude on its first line:

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

App sources live in `resources/apps/<id>/`: a manifest without `view`, the
entry program, and an optional `app.tsx` view with its `app.d.ts` contract and
`pocket.json`. `resources/apps/build.ts` compiles them into
`filesystem/resources/apps` and adds the `view` files to the manifest. It
needs a PocketJS checkout for the MicroTS compiler and font baker, so it is a
manual task and its outputs are committed:

```sh
POCKETJS_ROOT=~/src/pocketjs cargo plugin run apps --plugin app-manager
```
