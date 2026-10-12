# App Manager Plugin

- Plugin ID: `app-manager`
- Direct dependencies: `vm`, `ui`, `workflow`
- Provided typed capabilities: `AppManager`
- Required typed capabilities: `Vm` and `LuaPackageRegistry` from `vm`,
  `UiService` from `ui`, `WorkflowActionRegistry` from `workflow`
- Owned Components: none
- Plugin-owned tasks: one task that supervises up to `MAX_APP_RUNS` (3) app
  runs, their grace periods, and suspension
- Workflow Actions: `app.list`, `app.open`, `app.home`, `app.interactive`,
  `app.terminate`, `app.status`, `app.tap`
- Lua packages: `app`
- Events, Agent Tools: none
- Storage: private Plugin filesystem. Reads app containers; writes each app's
  data under `/media/data/<id>/`. Retains its Workflow Action and Lua package
  registrations for its lifetime.

## Apps

An app is a Lua program with an optional compiled MicroTS view, stored as an
app container. [`app-format.md`](app-format.md) defines the container, the
manifest, and the `app` Lua API; `schemas/app/manifest.json` checks the
manifest.

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
