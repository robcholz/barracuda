# App Format

This is the format of an app container, version 1. The App Manager loads any
container in this format from each of its locations, so a container written
by the image build, an installer, or an Agent is the same thing.
`schemas/app/manifest.json` is the machine-readable form of the manifest
rules below.

## Locations

| Location    | Directory               | Written by                                 |
| ----------- | ----------------------- | ------------------------------------------ |
| `installed` | `/media/apps/<id>/`     | the App Manager (private)                  |
| `bundled`   | `/resources/apps/<id>/` | the image build (private, read-only)       |
| `workspace` | `/workspace/media/apps/<id>/` | Agents and other Plugins (shared Workspace) |

All three are paths the App Manager's existing filesystem scope already
contains; the App Manager only reads the Workspace. When two containers share
an ID, the first location in the table wins, so a Workspace container never
replaces an installed or bundled app.

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
  }
}
```

| Field          | Required | Rule                                                        |
| -------------- | -------- | ----------------------------------------------------------- |
| `format`       | yes      | `1`                                                         |
| `id`           | yes      | 1–32 of `a-z`, `0-9`, `-`; equal to the directory name      |
| `name`         | yes      | display name, 1–64 characters                               |
| `version`      | yes      | the author's version string, 1–32 characters                |
| `entry`        | no       | the Lua program to run; defaults to `main.lua`              |
| `view`         | no       | omit for a headless app                                     |
| `view.program` | with view | the BVIR view program                                      |
| `view.styles`  | with view | the compiled stylesheet                                    |
| `view.fonts`   | no       | up to 16 baked font atlases; defaults to none               |

Every named file is a plain file name in the container: 1–64 of `A-Z`, `a-z`,
`0-9`, `_`, `.`, `-`, never `.` or `..`, and it must exist. Unknown fields
are rejected so a typo is reported, not ignored. The manifest is at most
4 KiB, the entry program 64 KiB, and the view files together 512 KiB.

A container that breaks a rule is not launchable. `app.list` reports it under
`invalid` with the location, directory name, and the problem, for example
`app.json: JSON schema validation failed at $.view.program: Pattern`.

## Program

The entry program runs on the VM like any Lua run, with one global added on
its first line, so line numbers in errors match the file:

```lua
APP = { id = "counter", ui = "view-7" } -- ui is nil for a headless app
```

An app with a view claims it once and then waits for its handlers:

```lua
local view = assert(require("ui").attach(APP.ui))
assert(view:set("count", 0))         -- a value the view declares
while true do
  local handler, arg = view:next()   -- nil once the view closes
  if handler == nil then break end
  -- update state, then view:set(...)
end
```

`view:set` and `ui.attach` return `nil, message` on failure. Values are
checked against the view's contract: the names and types its `app.d.ts`
declares. Apps have no standard input; a read gets end of file. The app ends
when the program returns or fails; the outcome and last output lines appear
in `app.status` as `last_exit`.

## Views

`view.program`, `view.styles`, and `view.fonts` are build outputs: the MicroTS
compiler turns `app.tsx` and its `app.d.ts` contract into View IR, `ui-link`
links it into BVIR, and the font baker produces the atlases. An Agent that
cannot run that toolchain writes a headless app, or copies the view files of
an existing build next to its own entry program.

## Example: a headless app an Agent can write

`/workspace/media/apps/hello/app.json`:

```json
{ "format": 1, "id": "hello", "name": "Hello", "version": "1" }
```

`/workspace/media/apps/hello/main.lua`:

```lua
print("hello from " .. APP.id)
```

Launch it with `app.launch` and `{"id":"hello"}`; `app.status` then shows
`last_exit.output` as `["hello from hello"]`.
