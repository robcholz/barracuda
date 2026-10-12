# App Plugin

Lists, launches, and supervises apps: Lua programs with optional MicroTS views,
installed, bundled, or written by Agents.

- Plugin ID: `app`
- Dependencies: `vm`, `ui`, `workflow`
- Settings: none
- Context resources: none
- Provided capabilities: `AppManager`
- Required capabilities: `Vm` and `LuaPackageRegistry` from `vm`, `UiService`
  from `ui`, `WorkflowActionRegistry` from `workflow`
- Workflow: Actions `app.list`, `app.open`, `app.home`, `app.interactive`,
  `app.terminate`, `app.status`, `app.tap`, see `action.md`; Events none
- Agent Tools: none
- Lua packages: `app`
- Web: none
- Tasks: one task that supervises up to `MAX_APP_RUNS` (3) app runs, their
  grace periods, and suspension
- Storage: app containers under the private `/media/apps` and `/resources/apps`
  and the shared `/workspace/media/apps` (read only); each app's key-value store
  and files under the private `/media/data/<id>/`

## Apps

An app is a Lua program with an optional compiled MicroTS view, stored as an
app container. [`app-format.md`](app-format.md) defines the container, the
manifest, and the `app` Lua API;
`crates/plugin/resources/app-manifest.json` checks the manifest.

App containers are carved out of this Plugin's own filesystem scope, which is
unchanged: installed apps under the private `/media/apps`, bundled apps under
the private `/resources/apps`, and apps that Agents create under the shared
`/workspace/media/apps`, which this Plugin only reads. An installed app
replaces a bundled one with the same ID; a Workspace container never replaces
either. Each app's data lives under the private `/media/data/<id>/`.

## Lifecycle

The system UI owns the gestures and calls `AppManager`; this Plugin owns the
states, the budgets, and what the program is told. Programs only respond.

```text
               open                     home / open another app
not running ──────────→ active ⇄ inactive ──────────────→ background
                          ↑  set_interactive                │ parked within GRACE
                          │                                 ↓
                          └──────────── open ───────────  suspended
terminate from any state → terminating → exits within GRACE, or is cancelled
```

| System UI action                          | `AppManager` call           | Program sees                         |
| ----------------------------------------- | --------------------------- | ------------------------------------ |
| Tap an icon                               | `open(id)`                  | launch, then `app:active`; or `app:foreground`, `app:active` when resuming |
| Pull down the control center, start a gesture | `set_interactive(false)` | `app:inactive`                     |
| Close the overlay                         | `set_interactive(true)`     | `app:active`                         |
| Swipe up to home                          | `home()`                    | `app:inactive`, `app:background`     |
| Close it in the app switcher              | `terminate(id)`             | `app:terminate`                      |
| Taps on the app                           | `tap(x, y)`                 | the view's handler call              |

- **Input.** Only an active app gets view calls. Calls queued before it stops
  taking input are dropped, so none arrives after `app:inactive`.
- **Suspension.** `GRACE` (1 s) after `app:background`, an app waiting in
  `app.events()` is suspended: its view tree is released and its values kept,
  and `open` rebuilds the tree from them. An app still running is cancelled.
  The suspended program costs no CPU but keeps its VM run and Lua heap.
- **Termination.** `terminate` sends `app:terminate`; the run is cancelled if
  it has not ended `GRACE` later. Suspended apps beyond `MAX_SUSPENDED` (2),
  and the least recently used suspended app when a launch needs a run, are
  terminated without another event, which is why programs save on
  `app:background`.
- **Headless apps** have no view, never enter the foreground, and run as
  services until they return or are terminated.
- **Exits.** `AppManager::last_exit` records how the latest run ended: the VM
  outcome, its diagnostic or the reason it was cancelled, and the last output
  lines. `AppManager::running` lists running apps, most recently used first,
  for the app switcher.

Apps run on at most `MAX_APP_RUNS` (3) of the VM's four execution slots, so
one stays free for Workflow and Agent runs.

## The `app` package

Every app program starts with `require("app").__bind("<ticket>");` on its first
line. The ticket names the app's port, the `Send + Sync` state the program
shares with this Plugin: the lifecycle queue, its view channel from the `ui`
Plugin, its timer, and its storage. A ticket binds once; other Lua runs
cannot claim an app.

The package gives the program its view as the `view` userdata over the `ui`
Plugin's `ViewChannel`. It converts Lua values to the view's typed values,
checked against the names and types of the app's `app.d.ts`, so a wrong name or
type fails with `nil, message` and never reaches the view; integral floats are
accepted for integer values, and nested tables are limited to 16 levels.
Handler arguments arrive as Lua scalars.

```lua
assert(view:set("count", 3))  -- write a value declared in app.d.ts
view:is_open()
view:close()
```

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
POCKETJS_ROOT=~/src/pocketjs cargo plugin run apps --plugin app
```
