# Device UI

Barracuda's device UI is built from three layers: **the system**, **system
apps**, and **installable apps**. Every screen, gesture, and app belongs to
exactly one of them, and the layer decides who owns it, what it may do, and
what happens when it fails. This document is authoritative for that split;
the design of each screen lives in the device UI design, and the contracts in
the Plugins' docs.

| | System | System app | Installable app |
| --- | --- | --- | --- |
| What it is | native Rust in the `shell` Plugin | an app container in the image | an app container the person installed |
| Where it lives | the firmware and the `shell` Plugin's `/resources/view` | the `app` Plugin's `/resources/apps/<id>` | the `app` Plugin's `/media/apps/<id>` |
| Runs as | the `shell` task | a Lua run with a MicroTS view, under the `app` lifecycle | the same |
| Lua packages | none | every registered package | `app` and the permissions it asked for at install |
| Removable | no | no; only app info | yes; app info and uninstall |
| When it fails | the device is broken | the system shows that it exited | the same |
| Examples | home, launcher, switcher, notification and control centers, launching, exited, install, app info | 设置 (Settings); planned: 会话, 工作流, 文件 | 计数器, 空气质量, 灯带 |

## Deciding the layer

Put a feature in the **system** when any of these holds:

- it must work when an app hangs or crashes, or when no app runs;
- it arbitrates between apps: what is on the display, which app is in front,
  which apps run;
- it grants trust: installing, uninstalling, and what an app may use;
- it answers input that works everywhere: edge swipes, back, long presses on
  system surfaces, and the Board's home and Agent keys.

Make it a **system app** when it is a full-screen experience the device
ships with, that needs packages an installable app could not get without
asking, and that may fail without taking the device down: Settings,
conversations, Workflows, files.

Everything else is an **installable app**: it is offered in the Workspace by
the portal, an Agent, or another Plugin, and the person installs it.

## The system

The `shell` Plugin is the system. It is the only caller that switches,
installs, and uninstalls apps for the person and the only owner of the
system gestures. Its view sits on the `ui` Plugin's system layer, above the
app layer: while an app is in the foreground and no system screen covers it,
the app's view shows; any system screen covers the app and the shell stops
the app's input (`AppManager::set_interactive`) until the screen closes.

The `ui` Plugin turns touch into taps, which press the view on the display,
and long presses and swipes, which go only to the system
(`UiService::next_gesture`). Apps never see system gestures, so no app can
take the person's way out.

Notices reach the person through the system's notification center, which
any Plugin posts to with the `shell` Plugin's `Notifications` capability and
Workflows with `shell.notify`. An app never draws over another app or over
the system.

## System apps

System apps are containers in the image, under the `app` Plugin's
`/resources/apps`. The `app` Plugin decides their kind from that location, so
nothing outside the image build can make a system app. They run under the
same lifecycle as any app (active, inactive, background, suspended,
terminating) and may `require` every registered Lua package without
declaring it. No installed app may take a system app's ID, and system apps
cannot be uninstalled.

## Installable apps

An installable app starts as a container offered in the Workspace under
`/workspace/media/apps/<id>`; it is not launchable there. Its manifest lists
`permissions`: the Lua packages it wants besides `app`. The system's install
screen shows them in plain words; when the person installs it, the `app`
Plugin copies the container into its private `/media/apps/<id>`, and from then
on every run of it may `require` only `app` and those packages. A Workflow or
Agent asks for an install with `app.install`; only the person completes it.
Uninstalling deletes the container and the app's data; the Workspace offer
stays until its author removes it.

## Fonts

Views carry their own baked font atlases, which live on the resources
partition and in RAM while the view is open. The system view bakes one text
size with every character the system itself shows and the names of the
bundled apps, and a display size with only its headline; apps bake the
characters in their own sources. Text with characters outside an atlas
shows a placeholder box. A shared system font is the follow-up that lets
installable apps' names in any script show in the launcher.
