# Shell Plugin

The device's system UI: the home screen, the launcher, the app switcher, the
notification and control centers, and the screens around an app's lifecycle.

- Plugin ID: `shell`
- Dependencies: `ui`, `app`, `workflow`, `time`, `wifi`
- Settings: none
- Context resources: none
- Provided capabilities: `Notifications`
- Required capabilities: `UiService` from `ui`, `AppManager` from `app`,
  `WorkflowActionRegistry` from `workflow`, `UtcClock` from `time`,
  `WifiControl` from `wifi`
- Workflow: Action `shell.notify`, see `action.md`; Events none
- Agent Tools: none
- Lua packages: none
- Web: none
- Tasks: one task that runs the system UI until the Plugin stops
- Storage: reads the compiled system view from its private
  `/resources/view`; no KV keys

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
| Home          | the clock, standby, and the one thing that needs attention: offline before an unread notice | at boot, when no app is in the foreground, and swiping right from the launcher |
| Apps          | every launchable app, a dot on running ones, and Workspace apps offered for installation, dimmed | swiping left on home, or right inside an app |
| Running       | the app switcher: each running app, its state, and a close button | swiping left in the launcher             |
| Notices       | the notification center: notices newest first, dismiss, clear all | swiping down from the top edge, anywhere; swipe up to close |
| Control       | Wi-Fi status, do not disturb, brightness                       | swiping up from the bottom edge, anywhere; swipe down to close |
| Launching     | the app's icon and name while it starts                        | tapping an app                              |
| Exited        | the app on the display ended unexpectedly; reopen or go home   | the foreground app failing or being cancelled |
| Install       | the offered app, its version, and its permissions in plain words; install or cancel | tapping an offered app, or an `app.install` request |
| Info          | an app's version and kind; installable apps list their permissions and can be uninstalled | long-pressing an app in the launcher |

Gestures are the `ui` Plugin's single-finger gestures: taps press the view on
the display, and long presses and swipes come to this Plugin through
`UiService::next_gesture`, never to an app. A long press in the launcher is
resolved with `UiSession::resolve_press`, so it selects the app a tap there
would open. Panels switch without animation, the design's reduced-motion
behavior.

| Gesture                     | Where              | Does                                   |
| --------------------------- | ------------------ | -------------------------------------- |
| Tap                         | anything pressable | open, toggle, confirm                  |
| Long press (0.5 s)          | an app icon        | app info; uninstall for installable apps |
| Swipe down from the top edge | anywhere          | notification center                    |
| Swipe up from the bottom edge | anywhere         | control center                         |
| Swipe left / right          | home, apps, running | move between them                     |
| Swipe right                 | inside an app      | back to the launcher; the app moves to the background |
| Swipe right                 | install, info, exited | cancel                              |

## Apps and their lifecycle

Tapping a launchable app calls `AppManager::open`. Tapping an offered app
shows the install screen; installing calls `AppManager::install`, declining
`AppManager::decline_install`. The info screen's uninstall calls
`AppManager::uninstall`, which refuses system apps, so their info screen
offers none. The switcher's close calls `AppManager::terminate`.

The shell follows `AppManager::changed`, so apps opened or closed by a
Workflow (`app.open`, `app.home`, `app.terminate`) move the screens the same
way. An app that fails while on the display gets the exited screen; one that
fails in the background, or a headless app, gets a notice instead. An
`app.install` request shows the install screen over whatever is up.

## Notices

`Notifications` holds up to `MAX_NOTICES` (8) notices of a title and a body,
each cut to `MAX_NOTICE_CHARS` (64) characters. Any Plugin may post one, and
Workflows post with `shell.notify`. A posted notice marks the home screen as
unread until the notification center opens; do not disturb hides the mark.

## The status bar

The clock is UTC from the `time` Plugin's `UtcClock`, redrawn every 15
seconds, and empty until the clock is synchronized. The home screen's
offline alert follows `WifiControl::status`; host Boards whose network the
host manages are always connected.

## Building the view

The view's sources live in `resources/view/`: `app.tsx` and its `app.d.ts`
contract. `resources/view/build.ts` compiles them into
`filesystem/resources/view` with the PocketJS compiler and bakes the fonts
from Noto Sans SC. The atlases hold ASCII and every other character in the
view, in this Plugin's Rust sources, and in `charset.txt`; text with other
characters, such as an installable app's name, shows placeholder boxes for
them. The build is a manual task and its outputs are committed:

```sh
POCKETJS_ROOT=~/src/pocketjs CJK_FONT_DIR=~/fonts cargo plugin run view --plugin shell
```
