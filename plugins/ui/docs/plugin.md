# UI Plugin

- Plugin ID: `ui`
- Direct dependency: `vm`
- Provided typed capabilities: `UiService`
- Required typed capabilities: `LuaPackageRegistry` from `vm`
- Owned Components: none
- Plugin-owned tasks: one UI task that applies Lua writes to open views
- Workflow Actions, Events, Agent Tools: none
- Storage: none; retains its `ui` Lua package registration for its lifetime

The Plugin runs compiled MicroTS views. A view is a BVIR program
(`view.bvir`), its compiled stylesheet (`styles.bin`) and baked font atlases
(`font-<slot>.bin`), produced from an app's `app.tsx` by the app build. The
crate `barracuda-ui-runtime` reads BVIR, mounts it into a vendored PocketJS
core tree, and re-evaluates the tree when values change.

`UiService::open` mounts a view and returns a `UiSession`. The session's
one-time ticket goes to exactly one Lua run, which claims the view:

```lua
local view = assert(require("ui").attach(APP.ui))
assert(view:set("count", 3))       -- write a value declared in app.d.ts
local handler, a, b = view:next()  -- wait for a handler call; nil once closed
view:close()
```

Lua values are checked against the view contract: names and types come from
the app's `app.d.ts`, so a wrong name or type fails with `nil, message` and
never reaches the tree. Writes keep only the latest value per slot. Handler
calls queue up to `MAX_EVENTS` per view; arrays written from Lua hold at most
`MAX_ARRAY_ITEMS` items.

Lua runs never touch the UI tree. They share only a critical-section mailbox
with the UI task, which keeps the `ui` package `Send + Sync`. The UI task
wakes on writes, applies them, and lays out every changed view.

Dropping the `UiSession` unmounts the view and ends it for Lua: `view:next()`
returns nothing and `view:set` fails. Closing from Lua is visible through
`UiSession::is_closed`. Revoking the package closes every view.

No display backend is connected yet. Views are laid out at
`DEFAULT_VIEWPORT`, and `UiSession::press_at` and `UiSession::texts` stand in
for touch input and rendering.
