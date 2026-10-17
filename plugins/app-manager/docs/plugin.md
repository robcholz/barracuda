# App Manager Plugin

Runs Lua apps with optional MicroTS views and, on a Board with a display,
shows them and the device's system UI: the home screen and the Agent's
states, the launcher, the app switcher, the notification and control
centers, and the screens around an app's lifecycle.

- Plugin ID: `app-manager`
- Dependencies: `vm`, `workflow`, `time`, `power`, `sound`
- Settings: none
- Context resources: peripheral slots `display.main` (`primary`, optional)
  and `touch.main` (`primary`, optional)
- Provided capabilities: `AppManager`, `Notifications`, `AgentStatus`,
  `Preferences`, `SystemFonts`, `SystemNetwork`
- Required capabilities: `Vm` and `LuaPackageRegistry` from `vm`,
  `WorkflowActionRegistry` from `workflow`, `UtcClock` from `time`,
  `Power` from `power`, `Volume` from `sound`
- Workflow: Actions `app.list`, `app.open`, `app.install`, `app.home`,
  `app.interactive`, `app.terminate`, `app.status`, `app.tap`, `app.notify`,
  `app.confirm`, and `app.agent`, see `action.md`; Events none
- Agent Tools: none
- Lua packages: `app` (ambient) and `notify` (stable, version 2), and each
  builtin app's package
- Web: none
- Tasks: one task that supervises up to `MAX_APP_RUNS` (3) app runs, answers
  builtin apps' package calls, and, with a display, draws views, reads
  touch, and runs the system UI
- Storage: installed apps under the private `/media/apps`, offers under the
  shared `/workspace/media/apps` (read only); each app's key-value store and
  files under the private `/media/data/<id>/`; the system view in
  `/resources/view`; preferences
  in `/data/preferences.json`

## Structure

The Plugin assembles two kits, its own crates:

| Crate | Owns | Knows |
| --- | --- | --- |
| `crates/appkit` | apps: containers, installs, the lifecycle, the `app` package, storage, builtin apps and their packages, the `app.*` app Actions | nothing about a display: given no UI, it runs headless apps and answers `NoDisplay` for apps with views |
| `crates/uikit` | views: the BVIR runtime, layers, the display and touch loop, gestures, the system fonts and icons | nothing about apps |
| `crates/plugin` | the entry point: connects the two, and the system UI | both |

On a Board without a display, no UI kit runs: the Plugin is an app manager
for headless apps. The device UI's three layers, the system, system apps,
and installable apps, are in
[`device-ui.md`](../../../.agents/docs/device-ui.md); this Plugin is the
system, and runs the other two.

## Apps

An app is a Lua program with an optional compiled MicroTS view, stored as an
app container. [`app-format.md`](app-format.md) defines the container, the
manifest, and the `app` Lua API;
`crates/appkit/resources/app-manifest.json` checks the manifest.

Where a container lives decides its `AppKind`, as
[`device-ui.md`](../../../.agents/docs/device-ui.md) defines:

- **System apps** are builtin apps that other Plugins ship, each at
  `/resources/app` in its own filesystem; the Plugin adds itself with
  `AppManager::add_builtin`, see below. Their runs may `require` every
  registered Lua package, `uninstall` refuses them, and no installed app may
  take their IDs.
- **Installable apps** are installed under the private `/media/apps`. Their
  runs get `LuaPackageScope::Only` the ambient packages and the
  packages their manifest names, through `Vm::run_scoped`.
- **Offers** are containers under the shared `/workspace/media/apps`, written
  by the portal, Agents, or other Plugins. They are not launchable; `open`
  answers `NotInstalled`. `AppManager::install` copies an offer, a
  directory or a `.bapp` package, into `/media/apps`, replacing an installed
  app at another version; it takes only what the pending request pinned
  (`Changed` otherwise) and, for an app installed from a signed package,
  only that signer's packages. `AppManager::install_offer` describes an
  offer as an install would take it, with its `digest`, its `signer`, and
  why it cannot be installed, if it cannot. The format is in
  [`app-format.md`](app-format.md#packages), and
  `AppManager::uninstall` deletes an installed app and its data. Only the
  system UI calls them, after asking the person; `AppManager::request_install`
  (the `app.install` Action) asks the system UI to show an offer, and
  `AppManager::install_request` and `AppManager::decline_install` are its side
  of that request.

Each app's data lives under the private `/media/data/<id>/`, keyed by ID, so
it survives updates.

## Builtin apps

A builtin app Plugin depends on this one and, while it registers, hands
`AppManager::add_builtin` a `BuiltinApp`: its ID, its filesystem, what the
system UI opens it for (`opens`, such as `wifi` for the control center's
Wi-Fi tile), and optionally a `BuiltinPackage`, a Lua package name with the
`BuiltinBackend` that answers it. It keeps the returned registration for its
life. The package is the app's way to the device: installable apps may never
ask for it, and a manifest that does is invalid.

A call, `require("settings").status()`, passes up to `MAX_BUILTIN_ARGS` (4)
arguments as strings and queues a job; this Plugin's task answers it with the
backend, which returns a JSON value Lua receives as tables, or a message Lua
receives as `nil, message`. At most `MAX_BUILTIN_CALLS` (8) calls wait at once,
and calls that wait on the person, such as a question, hold up no others.
`AppManager::serve_builtins` runs the answering loop.

## App API

[`app-api.md`](app-api.md) is the contract between installable apps and the
device. This Plugin hands `AppManager::register` the `Device` apps are
checked against: the Board's stable name, the display's size and shape from
the UI kit (`DisplayFeatures::ROUND` marks a round panel), and whether it
reads touch. Every listing reads the packages' tiers from
`LuaPackageRegistry::packages`, so a package Plugin registered after this
one counts, and fills each installable app's `incompatible`; `request_install`,
`install`, and `open` refuse an app that has one with `Incompatible`.
`AppManager::purpose` is the install screen's line for a package, as its
Plugin declared it in `LuaPackage::interface`; this Plugin names no other
Plugin's package and learns them all from the registry.

The `notify` package is this Plugin's: `AppManager::add_package`
registers it with the same bridge builtin apps' packages use, and its backend
posts to `Notifications`.

## Lifecycle

The system UI owns the gestures and calls `AppManager`;
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
| Swipe right in the app, on a deeper page  | `back()`                    | `app:back`                           |
| Swipe right out of the app, on its first page | `back()`, which calls `home()` | `app:inactive`, `app:background` |
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
shares with this Plugin: the lifecycle queue, its view channel from the UI
kit, its timer, and its storage. A ticket binds once; other Lua runs
cannot claim an app.

The package gives the program its view as the `view` userdata over the UI kit's
`ViewChannel`. It converts Lua values to the view's typed values,
checked against the names and types of the app's `app.d.ts`, so a wrong name or
type fails with `nil, message` and never reaches the view; integral floats are
accepted for integer values, and nested tables are limited to 16 levels.
Handler arguments arrive as Lua scalars.

```lua
assert(view:set("count", 3))  -- write a value declared in app.d.ts
view:is_open()
view:close()
```

## Views

The UI kit runs compiled MicroTS views. A view is a BVIR program
(`view.bvir`), its compiled stylesheet (`styles.bin`) and baked font atlases
(`font-<slot>.bin`), produced from MicroTS sources by the MicroTS compiler. The
crate `barracuda-ui-runtime` reads BVIR, mounts it into a vendored PocketJS
core tree, and re-evaluates the tree when values change.

`UiService::open` mounts an app's view on the app layer and
`UiService::open_on` a view on a chosen `Layer`; both return a `UiSession`,
kept by whoever presents the view. Its `ViewChannel` is the `Send + Sync` half for the code
that drives the view, on any task: `ViewChannel::set` writes a `ViewValue`,
and `ViewChannel::next` waits for the next handler call, whose arguments are
`ViewValue`s too. The UI kit knows nothing about Lua; the app kit converts the values.

Writes are checked against the view contract (`ViewChannel::value_type`):
an undeclared name fails with `ViewError::UnknownValue` and a value of another
type with `ViewError::Mismatch`, and neither reaches the tree. Writes keep only
the latest value per slot, and arrays hold at most `MAX_ARRAY_ITEMS` items.
Handler calls queue up to `MAX_EVENTS` per view, and `UiSession::clear_events`
drops the ones not yet received when the view stops taking input.

The owner never touches the UI tree. It shares only a critical-section mailbox
with the UI loop, which wakes on writes, applies them, and lays out every
changed view.

A view is a function of its values. `UiSession::suspend` releases the tree
(the bulk of a view's memory) and keeps the values; writes made meanwhile wait
in the mailbox. `UiSession::resume` rebuilds the tree from the assets and those
values, and refuses assets whose contract no longer matches.

Dropping the `UiSession` unmounts the view and ends it for the channel: it
stops delivering calls and writes fail with `ViewError::Closed`. Closing
through the channel is visible through `UiSession::is_closed`.

## The display

The Plugin takes the Board's primary display through its optional
`display.main` slot and lays every view out at the display's size, one view
pixel per display pixel.

Views sit on one of two layers. `Layer::App` holds apps' views and
`Layer::System` the system UI's; each layer shows at most one view, and the
display shows the system layer's view when it has one and the app layer's
otherwise (see [`device-ui.md`](../../../.agents/docs/device-ui.md)). An owner
puts its view on its layer with `UiSession::show`, which replaces the layer's
previous view, and takes it off with `UiSession::hide`; suspending or dropping
the session hides it too. So the system UI covers and uncovers an app without
the app's owner taking its view down. A display with no view on either layer
is blank. `UiService::showing` reports what is on the display, and
`UiService::set_brightness` sets the panel's brightness.

`UiService::set_screen_timeout` turns the panel off after a time without a
touch, on Boards with touch; the touch that turns it back on reaches no view
and makes no gesture. `UiService::wake` turns it on from code.

Only what changed is drawn. After each change the task asks the PocketJS damage
tracker which regions of the view's DrawList differ from the last frame,
rasterizes them as RGB565, the display API's pixel format, in strips of 16
rows, draws each strip, and flushes the changed bounds. No
framebuffer is kept. While a frame still changes, as during an animation, the
task steps frames at 60 per second; a settled view costs nothing.

The optional `touch.main` slot takes the Board's touch controller. While a
view is on the display, and until a contact that began ends, the task polls
it every frame, scaling the controller's span to the display, and classifies
each contact:

| Contact | Becomes |
| --- | --- |
| lifts within 12 px of where it began | a tap: presses the view on the display at the start, if it takes input (`UiSession::show(true)`) |
| rests within 12 px for 0.5 s | `Gesture::LongPress`, reported while the finger is still down; the contact taps nothing |
| travels 40 px or more along one axis, then lifts | `Gesture::Swipe` with its `Direction` and, when it started within 12% of the side it travelled away from, that `Edge` |

Gestures reach no view: the system UI waits for them with
`UiService::next_gesture`, which keeps the latest `MAX_GESTURES` (8) while
nobody waits. To act on what a long press is over, the system UI asks its own
session with `UiSession::resolve_press`, which returns the handler call a press
there would make without delivering it.

Handler calls from taps arrive through the view's channel as from
`UiSession::press_at`, which presses from code and works without a display.
`UiSession::texts` reads a view's text the same way, and
`UiSession::advance` steps its transitions as the display would.

### Apps on the display

App views sit on the app layer. After every transition this
Plugin calls `UiSession::show` for the foreground app's view, with touches
reaching it only while the app is active, and hides app views when no app is
in the foreground. The system UI's view sits on the system layer above, so it
covers an app without this Plugin's involvement. Taps on the display press the
view directly through the UI kit. `AppManager::tap` and
`AppManager::texts` do the same from code, for Workflows (`app.tap`) and
tests, and work without a display.

## Fonts and icons

The system fonts are font atlases another Plugin ships, `system-fonts` on
device Boards, because they are about 2.4 MB and a Board without a display
needs none. That Plugin gives the `SystemFonts` capability a `FontAtlases`
source while registering, and the task loads the atlases through it before
the first view opens, never reading that Plugin's files; `UiService::ready`
waits for them. A view that ships no atlas for a font slot draws with the
system font for it, so the system UI and the system apps share one set and
ship none. Without a Plugin that ships them, views draw only with their own
atlases. The `system-fonts` Plugin's
[`plugin.md`](../../system-fonts/docs/plugin.md) says what they hold.

The system icon set is one font, `resources/icons/barracuda-icons.otf`,
built from `resources/icons/svg` by `resources/icons/build.py`. Each icon is a
glyph per color layer at fixed code points (`codepoints.json`), stacked and
tinted by a view; `icons::icon` lists an icon's layers for Rust code. The
system fonts carry the icons at 16 px regular and 18 px bold, and the app
icons at 36 px.

Text without a size or weight class uses font slot 0 in the UI core, while the
PocketJS compiler means its 16 px default and bakes that as slot 2. When a
view ships slot 2 but no slot 0, the runtime serves slot 0 from it.

A `UiService` made without a display, as in tests, lays views out at
`DEFAULT_VIEWPORT` and draws nothing.

## The system UI

The system UI opens one view on the system layer. While an app is in the
foreground and no system screen covers it, the view is hidden and the app's
view shows. Every system screen covers the foreground app and stops its
input (`AppManager::set_interactive(false)`) until the screen closes. It is
the only part of the device that installs, uninstalls, and switches apps for
the person.

### Screens and gestures

| Screen        | Shows                                                         | Gets there                                  |
| ------------- | ------------------------------------------------------------- | ------------------------------------------- |
| Home          | the clock and status icons, the Agent in its state, and one card: offline, else a pending question, else what was just done | at boot, when no app is in the foreground, and swiping right from the launcher |
| Apps          | the honeycomb launcher: builtin apps first, then installed apps, then Workspace apps offered for installation, dimmed; a dot on running ones. A page holds the rows that fit the display (2-3-2 on the 360 px round panel); further pages are a swipe up away, with a dot per page | swiping left on home, or right inside an app at its first page |
| Menu          | an app's name over the dimmed launcher: app info, and uninstall for installable apps | long-pressing an app in the launcher |
| Running       | the app switcher: each running app, its state, and a close button | swiping left in the launcher             |
| Notices       | the notification center: questions first with allow and deny, then notices newest first, dismiss, clear all | swiping down from the top edge, anywhere; swipe up to close |
| Control       | tiles for Wi-Fi, microphone, mute, do not disturb, brightness, and the portal; the brightness slider; the portal's address | swiping up from the bottom edge, anywhere; swipe down to close |
| Launching     | the app's icon and name while it starts                        | tapping an app                              |
| Exited        | the app on the display ended unexpectedly; reopen or go home   | the foreground app failing or being cancelled |
| Install       | the offered app and what it asks for in its Plugins' words; install or cancel, or, for an app this device cannot run, why, with install disabled | tapping an offered app, or an `app.install` request |
| Info          | an app's version and kind; installable apps list their permissions | the menu's app info                    |

Home shows the Agent's `AgentStatus`: standby; listening, with a level meter
and what it hears; thinking, with the question; and answering, with the
question and the answer, which returns to standby after nine seconds or a
tap. An answered question shows as done for a moment and on the home card for
30 seconds.

Gestures are the UI kit's single-finger gestures: taps press the view on
the display, and long presses and swipes come to this Plugin through
`UiService::next_gesture`, never to an app. A long press is resolved with
`UiSession::resolve_press`, so it selects what a tap there would. Home, the
launcher, and the switcher slide sideways and the panels slide in from their
edge, over 280 ms.

| Gesture                     | Where              | Does                                   |
| --------------------------- | ------------------ | -------------------------------------- |
| Tap                         | anything pressable | open, toggle, confirm                  |
| Long press (0.5 s)          | an app icon        | the app menu                           |
| Long press                  | the Wi-Fi tile     | the builtin app that opens for `wifi`  |
| Swipe down from the top edge | anywhere          | notification center                    |
| Swipe up from the bottom edge | anywhere         | control center                         |
| Swipe left / right          | home, apps, running | move between them                     |
| Swipe up / down             | apps               | the launcher's next / previous page    |
| Swipe right                 | inside an app      | back (`AppManager::back`): the app's previous page, or the launcher from its first page |
| Swipe right                 | menu, install, info, exited | cancel                         |

### Apps and their lifecycle

Tapping a launchable app calls `AppManager::open`. Tapping an offered app
shows the install screen; installing calls `AppManager::install`, declining
`AppManager::decline_install`. The menu's uninstall calls
`AppManager::uninstall`, which refuses system apps, so their menu offers
none. The switcher's close calls `AppManager::terminate`.

The system UI follows `AppManager::changed`, so apps opened or closed by a
Workflow (`app.open`, `app.home`, `app.terminate`) move the screens the same
way. An app that fails while on the display gets the exited screen; one that
fails in the background, or a headless app, gets a notice instead. An
`app.install` request shows the install screen over whatever is up.

### Notices and questions

`Notifications` holds up to `MAX_NOTICES` (8) notices of a title, a body, and
an optional system icon, each text cut to `MAX_NOTICE_CHARS` (64)
characters. Any Plugin may post one, and Workflows post with `app.notify`.
A posted notice marks the home screen as unread until the notification
center opens; do not disturb hides the mark.

`Notifications::ask` posts a question instead; `Notifications::answer` waits
until the person allows or denies it on its card, and answers `None` when it
is dismissed or dropped. Workflows ask with `app.confirm`.

### Preferences

`Preferences` holds the brightness, the volume, mute, the microphone, do not
disturb, and the screen timeout (`SCREEN_TIMEOUTS`). The control center and
builtin apps, such as Settings, change the same values.
The system UI applies the brightness and the screen timeout to the display with
`UiService::set_brightness` and `UiService::set_screen_timeout`, and saves the
values after every change. It puts the volume and mute on the speaker with
`Volume::set`: every app's sound is scaled by the volume, and mute
silences it. The microphone is the person's choice for the Agent's voice.
The control center shows the battery's charge from `Power::battery`
on a Board whose power monitor is a fuel gauge, and nothing on others.

### The status bar

The clock is UTC from the `time` Plugin's `UtcClock`, redrawn every 15
seconds: from the Board's real-time clock at boot when it has one, then from
network time, and empty on a Board without one until network time arrives. Do not disturb and
offline show as icons beside it. The network is whatever a Plugin shows
with `SystemNetwork::show`, a `NetworkSource`: `system-wifi` shows Wi-Fi. It
drives the home screen's offline card, the control center's Wi-Fi tile, and
the portal tile, which opens the source's setup hotspot and shows its
`hotspot_hint`. A device that shows no network, such as one built without
networking, has no Wi-Fi or portal tile and is never offline.

## Building

The Plugin's build inputs live in `resources/` and their outputs, committed,
in `filesystem/resources/` and `crates/appkit/tests/fixtures/`. Each task
needs a PocketJS checkout (`POCKETJS_ROOT`) for the MicroTS compiler and font
baker, and the examples Noto Sans SC (`CJK_FONT_DIR`):

| Task | Builds |
| --- | --- |
| `icons` | the icon font and tables from `resources/icons/svg` |
| `view` | the system view from `resources/view/app.tsx` and its `app.d.ts` contract; it ships no atlases |
| `examples` | the example installable apps in `resources/examples/` into `crates/appkit/tests/fixtures/` |

`resources/build-app.ts` builds an app container from its sources; builtin
app Plugins run it on their own `resources/app`. Builtin apps ship no fonts
and draw with the system fonts. Examples bake their own, as an app from
elsewhere would: Noto Sans SC for characters beyond ASCII, system icons named
by `\uE0xx` escapes from the icon font, and only digits and icons at sizes of
24 px and up. Icons in a view are escapes such as `"\uE008"`, whose code
points are fixed in `resources/icons/codepoints.json`.

```sh
POCKETJS_ROOT=~/src/pocketjs CJK_FONT_DIR=~/fonts cargo plugin run fonts --plugin app-manager
```
