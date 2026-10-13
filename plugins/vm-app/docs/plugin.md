# App Plugin

Lists, installs, launches, and supervises apps: Lua programs with optional
MicroTS views. It runs the device UI's system apps and installable apps; the
`shell` Plugin is the system that drives it.

- Plugin ID: `app`
- Dependencies: `vm`, `ui`, `workflow`
- Settings: none
- Context resources: none
- Provided capabilities: `AppManager`
- Required capabilities: `Vm` and `LuaPackageRegistry` from `vm`, `UiService`
  from `ui`, `WorkflowActionRegistry` from `workflow`
- Workflow: Actions `app.list`, `app.open`, `app.install`, `app.home`,
  `app.interactive`, `app.terminate`, `app.status`, `app.tap`, see
  `action.md`; Events none
- Agent Tools: none
- Lua packages: `app`
- Web: none
- Tasks: one task that supervises up to `MAX_APP_RUNS` (3) app runs, their
  grace periods, and suspension
- Storage: system apps under the private `/resources/apps`, installed apps
  under the private `/media/apps`, offers under the shared
  `/workspace/media/apps` (read only); each app's key-value store and files
  under the private `/media/data/<id>/`

## Apps

An app is a Lua program with an optional compiled MicroTS view, stored as an
app container. [`app-format.md`](app-format.md) defines the container, the
manifest, and the `app` Lua API;
`crates/plugin/resources/app-manifest.json` checks the manifest.

Where a container lives decides its `AppKind`, as
[`device-ui.md`](../../../.agents/docs/device-ui.md) defines:

- **System apps** are bundled in the image under the private
  `/resources/apps`. Their runs may `require` every registered Lua package,
  `uninstall` refuses them, and no installed app may take their IDs. The image
  ships 设置 (`settings`), which reads `systeminfo`.
- **Installable apps** are installed under the private `/media/apps`. Their
  runs get `LuaPackageScope::Only` the `app` package and the `permissions`
  their manifest asks for, through `Vm::run_scoped`.
- **Offers** are containers under the shared `/workspace/media/apps`, written
  by the portal, Agents, or other Plugins. They are not launchable; `open`
  answers `NotInstalled`. `AppManager::install` copies an offer into
  `/media/apps`, replacing an installed app at another version, and
  `AppManager::uninstall` deletes an installed app and its data. Only the
  system UI calls them, after asking the person; `AppManager::request_install`
  (the `app.install` Action) asks the system UI to show an offer, and
  `AppManager::install_request` and `AppManager::decline_install` are its side
  of that request.

Each app's data lives under the private `/media/data/<id>/`, keyed by ID, so
it survives updates.

## Lifecycle

The system UI, the `shell` Plugin, owns the gestures and calls `AppManager`;
this Plugin owns the states, the budgets, and what the program is told.
Programs only respond. `AppManager::changed` wakes the system UI after every
transition, exit, install, and install request, so apps that Workflows open
or close move its screens too.

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
| Pull down a panel, or any system screen covers the app | `set_interactive(false)` | `app:inactive`         |
| Close the panel                           | `set_interactive(true)`     | `app:active`                         |
| Swipe right out of the app                | `home()`                    | `app:inactive`, `app:background`     |
| Close it in the app switcher              | `terminate(id)`             | `app:terminate`                      |
| Uninstall it from its info screen         | `uninstall(id)`             | `app:terminate`                      |
| Touch the app's view                      | none; `tap(x, y)` from code | the view's handler call              |

- **Input.** Only an active app gets view calls; an inactive app's view stays
  on the display but ignores touches. Calls queued before it stops taking
  input are dropped, so none arrives after `app:inactive`.
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
  outcome, its diagnostic or the reason it was cancelled, the last output
  lines, and whether the app was in the foreground, so the system UI can say
  that the app on the display exited. `AppManager::running` lists running apps, most recently used first,
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

## The display

App views sit on the `ui` Plugin's app layer. After every transition this
Plugin calls `UiSession::show` for the foreground app's view, with touches
reaching it only while the app is active, and hides app views when no app is
in the foreground. The system UI's view sits on the system layer above, so it
covers an app without this Plugin's involvement. Taps on the display press the
view directly through the `ui` Plugin. `AppManager::tap` and
`AppManager::texts` do the same from code, for Workflows (`app.tap`) and
tests, and work without a display.

## Building bundles

System app sources live in `resources/apps/<id>/` and example installable apps
in `resources/examples/<id>/`: a manifest without `view`, the entry program,
and an optional `app.tsx` view with its `app.d.ts` contract and `pocket.json`.
`resources/apps/build.ts` compiles system apps into `filesystem/resources/apps`
and examples into `crates/plugin/tests/fixtures`, and adds the `view` files to
each manifest. It needs a PocketJS checkout for the MicroTS compiler and font
baker; an app whose sources show characters beyond ASCII bakes its fonts from
Noto Sans SC, found in `CJK_FONT_DIR`. It is a manual task and its outputs are
committed:

```sh
POCKETJS_ROOT=~/src/pocketjs CJK_FONT_DIR=~/fonts cargo plugin run apps --plugin app
```
