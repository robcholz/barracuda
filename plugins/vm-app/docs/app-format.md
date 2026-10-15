# App Format

This is the format of an app container, version 1. The `app` Plugin loads any
container in this format from each of its locations, so a container written
by the image build, the portal, or an Agent is the same thing; where it lives
decides what kind of app it is (see
[`device-ui.md`](../../../.agents/docs/device-ui.md)).
`crates/plugin/resources/app-manifest.json` is the machine-readable form of the manifest
rules below.

## Locations

| Location    | Directory                     | Written by                            | Holds |
| ----------- | ----------------------------- | ------------------------------------- | ----- |
| `bundled`   | `/resources/apps/<id>/`       | the image build (private, read-only)  | system apps |
| `installed` | `/media/apps/<id>/`           | the `app` Plugin on install (private) | installable apps |
| `workspace` | `/workspace/media/apps/<id>/` | the portal, Agents, other Plugins (shared Workspace) | apps offered for installation |

All three are paths the `app` Plugin's existing filesystem scope already
contains; the `app` Plugin only reads the Workspace. Bundled and installed
apps are launchable; a Workspace container is an offer. Installing it copies
the manifest and the files it names into `/media/apps/<id>/`, replacing an
installed app at another version. A system app's ID belongs to it: no
container elsewhere may use it, and `app.list` reports one that does as
invalid.

## Container

A container is a directory named after the app ID, holding a manifest and the
files it names, side by side:

```text
<id>/
  app.json          the manifest
  main.lua          the entry program (or the file `entry` names)
  view.bvir         optional view program        ┐
  styles.bin        its compiled stylesheet      ├ named by `view`
  font-1.bin …      its baked font atlases       ┘
```

Other files are ignored. Directories whose names start with `.` are skipped.

## Manifest

```json
{
  "format": 1,
  "id": "counter",
  "name": "计数器",
  "version": "0.1.0",
  "entry": "main.lua",
  "view": {
    "program": "view.bvir",
    "styles": "styles.bin",
    "fonts": ["font-1.bin", "font-2.bin", "font-12.bin"]
  },
  "permissions": ["led_strip", "http"]
}
```

| Field          | Required | Rule                                                        |
| -------------- | -------- | ----------------------------------------------------------- |
| `format`       | yes      | `1`                                                         |
| `id`           | yes      | 1–32 of `a-z`, `0-9`, `-`; equal to the directory name      |
| `name`         | yes      | display name, 1–64 characters                               |
| `version`      | yes      | the author's version string, 1–32 characters                |
| `icon`         | no       | a system icon's name, such as `lamp`, shown on the launcher; without one it shows the name's first character |
| `entry`        | no       | the Lua program to run; defaults to `main.lua`              |
| `view`         | no       | omit for a headless app                                     |
| `view.program` | with view | the BVIR view program                                      |
| `view.styles`  | with view | the compiled stylesheet                                    |
| `view.fonts`   | no       | up to 16 baked font atlases; defaults to none               |
| `permissions`  | no       | up to 16 distinct Lua package names besides `app`, such as `gpio` or `http`; defaults to none |

Every named file is a plain file name in the container: 1–64 of `A-Z`, `a-z`,
`0-9`, `_`, `.`, `-`, never `.` or `..`, and it must exist. Unknown fields
are rejected so a typo is reported, not ignored. The manifest is at most
4 KiB, the entry program 64 KiB, and the view files together 512 KiB.

An installable app's runs can `require` only `app` and its `permissions`; a
package it did not ask for is not there, and `require` fails. The system UI
lists the permissions in plain words before the person installs the app. A
system app may `require` every registered package, so its manifest needs
none. `system`, the package that runs the device's settings, is only for
system apps: an installable app that asks for it is not valid.

A container that breaks a rule is not launchable. `app.list` reports it under
`invalid` with the location, directory name, and the problem, for example
`app.json: JSON schema validation failed at $.view.program: Pattern`.

## Program

The entry program runs on the VM like any Lua run, with the `app` package
bound to this app. The binding shares the program's first line, so line
numbers in errors match the file. Apps have no standard input; a read gets
end of file.

### The event loop

Every app is one loop over `app.events()`. View calls, lifecycle events, and
timer ticks arrive in that one stream, in order:

```lua
local app = require("app")
local view = app.view()                -- nil for a headless app

for event, a, b in app.events() do
  if event == "app:background" then save()
  elseif event == "app:terminate" then save() break
  elseif event == "increment" then ...  -- a view call: handler name, then arguments
  end
end
```

| Event            | When                                                                 |
| ---------------- | -------------------------------------------------------------------- |
| `app:active`     | The app is in the foreground and takes input. First event of a view app. |
| `app:inactive`   | Still visible but not taking input: an overlay covers it or a transition runs. No view call follows until `app:active`. |
| `app:background` | It left the foreground. Save now: a suspended app may be terminated without another event. |
| `app:foreground` | It came back from the background; `app:active` follows.              |
| `app:terminate`  | It is asked to exit. Return within the grace period or be cancelled. |
| `app:tick`       | The interval set with `app.events{ every = ms }` passed (at least 100 ms). Not delivered in the background or while suspended. |
| `app:back`       | The person swiped back while the app reported a page deeper than its first with `app.navigation`. Show the previous page and report its depth. |

After `app:background` the program must be back in `app.events()` within the
grace period (1 s); it is then suspended: it uses no CPU, and its view tree is
released and rebuilt from its values when it returns. A program still busy
when the grace period ends is cancelled, as is one that ignores
`app:terminate`. The loop ends after `app.exit()`; the program then returns.

Headless apps never enter the foreground. They run as services and only see
`app:tick` and `app:terminate`.

### API

| Call | Result |
| ---- | ------ |
| `app.id`, `app.name`, `app.version` | From the manifest. |
| `app.view()` | The view, or `nil` for a headless app. |
| `app.events([{ every = ms }])` | The event iterator; `every` starts a timer. |
| `app.exit()` | Ends the event loop. |
| `app.navigation(depth)` | Reports how many pages deep the app is; 0, the default, is its first page. A back swipe delivers `app:back` while it is above 0, and leaves the app at 0. |
| `view:set(name, value)` | Writes a value the view's `app.d.ts` declares; checked against its type. |
| `view:is_open()`, `view:close()` | The view's state; closing ends view calls. |
| `app.kv.get(key)` | The stored value, or `nil`. |
| `app.kv.set(key, value)` | Stores booleans, numbers, strings, and tables that are arrays or have string keys; `nil` deletes. |
| `app.kv.delete(key)`, `app.kv.keys()` | Delete a key; list keys, sorted. |
| `app.fs.read(path)`, `app.fs.write(path, data)`, `app.fs.append(path, data)` | Whole-file access; writes replace atomically. |
| `app.fs.list([dir])` | Sorted names; directories end in `/`. |
| `app.fs.exists(path)`, `app.fs.remove(path)`, `app.fs.mkdir(path)` | Plain file management. |

Failures return `nil` and a message instead of raising, so `assert(...)` turns
them into errors. Keys are 1–64 bytes and the store holds at most 16 KiB of
JSON. Files are at most 64 KiB; paths are relative to the app's own file
tree, at most 8 names deep, with no `.` or `..`.

Storage belongs to the app ID, not the container: it lives in the App
Manager's private `/media/data/<id>/` (`kv.json` and `files/`), survives
updates and restarts, and is never visible to other apps.

## Views

`view.program`, `view.styles`, and `view.fonts` are build outputs: the MicroTS
compiler turns `app.tsx` and its `app.d.ts` contract into View IR, `ui-link`
links it into BVIR, and the font baker produces the atlases. An Agent that
cannot run that toolchain writes a headless app, or copies the view files of
an existing build next to its own entry program.

## Example: a headless app an Agent can offer

`/workspace/media/apps/hello/app.json`:

```json
{ "format": 1, "id": "hello", "name": "Hello", "version": "1" }
```

`/workspace/media/apps/hello/main.lua`:

```lua
local app = require("app")
local runs = (app.kv.get("runs") or 0) + 1
assert(app.kv.set("runs", runs))
print("hello from " .. app.id .. ", run " .. runs)
```

Offer it with `app.install` and `{"id":"hello"}`: the device shows the
install screen, and once the person installs it, open it with `app.open` and
`{"id":"hello"}`; `app.status` then shows `last_exit.output` as
`["hello from hello, run 1"]`. `crates/plugin/tests/fixtures/` holds built
example apps with views, each ready to copy into `/workspace/media/apps/<id>/`:
`air` (空气质量, no packages), `lamp` (灯带, `led_strip`), `camera` (相机,
`camera`), and `counter` (计数器, no packages).
