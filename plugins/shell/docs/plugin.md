# Shell Plugin

The device's system UI: the home screen and the Agent's states, the
launcher, the app switcher, the notification and control centers, and the
screens around an app's lifecycle.

- Plugin ID: `shell`
- Dependencies: `ui`, `app`, `workflow`, `time`, `wifi`
- Settings: none
- Context resources: none
- Provided capabilities: `Notifications`, `AgentStatus`, `Preferences`
- Required capabilities: `UiService` from `ui`, `AppManager` from `app`,
  `WorkflowActionRegistry` from `workflow`, `UtcClock` from `time`,
  `WifiControl` from `wifi`
- Workflow: Actions `shell.notify`, `shell.confirm`, and `shell.agent`, see
  `action.md`; Events none
- Agent Tools: none
- Lua packages: none
- Web: none
- Tasks: one task that runs the system UI until the Plugin stops
- Storage: reads the compiled system view from its private
  `/resources/view`; keeps `Preferences` in `/data/preferences.json`; no KV
  keys

## The system layer

The device UI has three layers, defined in
[`device-ui.md`](../../../.agents/docs/device-ui.md): the system, system
apps, and installable apps. This Plugin is the system. It is not an app: it
cannot be closed, suspended, replaced, or uninstalled, and it keeps working
when an app hangs or fails. It is the only part of the device that installs,
uninstalls, and switches apps for the person, and that decides what is on
the display.

It opens one view on the `ui` Plugin's system layer. While an app is in the
foreground and no system screen covers it, the view is hidden and the app's
view shows. Every system screen covers the foreground app; the shell then
calls `AppManager::set_interactive(false)` and restores input when the
screen closes.

## Screens and gestures

| Screen        | Shows                                                         | Gets there                                  |
| ------------- | ------------------------------------------------------------- | ------------------------------------------- |
| Home          | the clock and status icons, the Agent in its state, and one card: offline, else a pending question, else what was just done | at boot, when no app is in the foreground, and swiping right from the launcher |
| Apps          | the honeycomb launcher: Sessions, Workflows, Files, and Settings first, then installed apps, then Workspace apps offered for installation, dimmed; a dot on running ones | swiping left on home, or right inside an app at its first page |
| Menu          | an app's name over the dimmed launcher: app info, and uninstall for installable apps | long-pressing an app in the launcher |
| Running       | the app switcher: each running app, its state, and a close button | swiping left in the launcher             |
| Notices       | the notification center: questions first with allow and deny, then notices newest first, dismiss, clear all | swiping down from the top edge, anywhere; swipe up to close |
| Control       | tiles for Wi-Fi, microphone, mute, do not disturb, brightness, and the portal; the brightness slider; the portal's address | swiping up from the bottom edge, anywhere; swipe down to close |
| Launching     | the app's icon and name while it starts                        | tapping an app                              |
| Exited        | the app on the display ended unexpectedly; reopen or go home   | the foreground app failing or being cancelled |
| Install       | the offered app and its permissions in plain words; install or cancel | tapping an offered app, or an `app.install` request |
| Info          | an app's version and kind; installable apps list their permissions | the menu's app info                    |

Home shows the Agent's `AgentStatus`: standby; listening, with a level meter
and what it hears; thinking, with the question; and answering, with the
question and the answer, which returns to standby after nine seconds or a
tap. An answered question shows as done for a moment and on the home card for
30 seconds.

Gestures are the `ui` Plugin's single-finger gestures: taps press the view on
the display, and long presses and swipes come to this Plugin through
`UiService::next_gesture`, never to an app. A long press is resolved with
`UiSession::resolve_press`, so it selects what a tap there would. Home, the
launcher, and the switcher slide sideways and the panels slide in from their
edge, over 280 ms.

| Gesture                     | Where              | Does                                   |
| --------------------------- | ------------------ | -------------------------------------- |
| Tap                         | anything pressable | open, toggle, confirm                  |
| Long press (0.5 s)          | an app icon        | the app menu                           |
| Long press                  | the Wi-Fi tile     | Settings                               |
| Swipe down from the top edge | anywhere          | notification center                    |
| Swipe up from the bottom edge | anywhere         | control center                         |
| Swipe left / right          | home, apps, running | move between them                     |
| Swipe right                 | inside an app      | back (`AppManager::back`): the app's previous page, or the launcher from its first page |
| Swipe right                 | menu, install, info, exited | cancel                         |

## Apps and their lifecycle

Tapping a launchable app calls `AppManager::open`. Tapping an offered app
shows the install screen; installing calls `AppManager::install`, declining
`AppManager::decline_install`. The menu's uninstall calls
`AppManager::uninstall`, which refuses system apps, so their menu offers
none. The switcher's close calls `AppManager::terminate`.

The shell follows `AppManager::changed`, so apps opened or closed by a
Workflow (`app.open`, `app.home`, `app.terminate`) move the screens the same
way. An app that fails while on the display gets the exited screen; one that
fails in the background, or a headless app, gets a notice instead. An
`app.install` request shows the install screen over whatever is up.

## Notices and questions

`Notifications` holds up to `MAX_NOTICES` (8) notices of a title, a body, and
an optional system icon, each text cut to `MAX_NOTICE_CHARS` (64)
characters. Any Plugin may post one, and Workflows post with `shell.notify`.
A posted notice marks the home screen as unread until the notification
center opens; do not disturb hides the mark.

`Notifications::ask` posts a question instead; `Notifications::answer` waits
until the person allows or denies it on its card, and answers `None` when it
is dismissed or dropped. Workflows ask with `shell.confirm`.

## Preferences

`Preferences` holds the brightness, the volume, mute, the microphone, do not
disturb, and the screen timeout (`SCREEN_TIMEOUTS`). The control center and
the Settings app, through the `system` Lua package, change the same values.
The shell applies the brightness and the screen timeout to the display with
`UiService::set_brightness` and `UiService::set_screen_timeout`, and saves the
values after every change. The volume is kept for audio output; mute and the
microphone are the person's choice for the Agent's voice.

## The status bar

The clock is UTC from the `time` Plugin's `UtcClock`, redrawn every 15
seconds, and empty until the clock is synchronized. Do not disturb and
offline show as icons beside it. The home screen's offline card follows
`WifiControl::status`; host Boards whose network the host manages are always
connected. The portal tile opens the Wi-Fi setup access point
(`SETUP_AP_SSID`) and shows its address.

## Building the view

The view's sources live in `resources/view/`: `app.tsx` and its `app.d.ts`
contract. `resources/view/build.ts` compiles them into
`filesystem/resources/view` with the PocketJS compiler. The view ships no
font atlases: it draws with the `ui` Plugin's system fonts, which hold every
character in this Plugin's view and Rust sources. Icons are escapes such as
`""` naming glyphs of the system icon font, whose code points are fixed
in the `ui` Plugin's `resources/icons/codepoints.json`. The build is a manual
task and its outputs are committed:

```sh
POCKETJS_ROOT=~/src/pocketjs cargo plugin run view --plugin shell
```
