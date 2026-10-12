# UI Plugin

- Plugin ID: `ui`
- Direct dependencies: none
- Provided typed capabilities: `UiService`
- Required typed capabilities: none
- Owned Components: none
- Plugin-owned tasks: one UI task that applies Lua writes to open views
- Workflow Actions, Events, Agent Tools, Lua packages: none
- Storage: none

The Plugin runs compiled MicroTS views. A view is a BVIR program
(`view.bvir`), its compiled stylesheet (`styles.bin`) and baked font atlases
(`font-<slot>.bin`), produced from an app's `app.tsx` by the app build. The
crate `barracuda-ui-runtime` reads BVIR, mounts it into a vendored PocketJS
core tree, and re-evaluates the tree when values change.

`UiService::open` mounts a view and returns a `UiSession`, owned by whoever
presents the view, such as the App Manager. Its `ViewChannel` is the `Send +
Sync` half a Lua package hands to the program that owns the view, wrapped as
the `LuaView` userdata:

```lua
assert(view:set("count", 3))  -- write a value declared in app.d.ts
view:is_open()
view:close()
```

Handler calls reach the program through `ViewChannel::next`; the App Manager
merges them into the app's single event loop. This Plugin registers no Lua
package of its own.

Lua values are checked against the view contract: names and types come from
the app's `app.d.ts`, so a wrong name or type fails with `nil, message` and
never reaches the tree. Writes keep only the latest value per slot. Handler
calls queue up to `MAX_EVENTS` per view, and `UiSession::clear_events` drops
the ones not yet received when the view stops taking input. Arrays written
from Lua hold at most `MAX_ARRAY_ITEMS` items.

Lua never touches the UI tree. It shares only a critical-section mailbox with
the UI task, which wakes on writes, applies them, and lays out every changed
view.

A view is a function of its values. `UiSession::suspend` releases the tree
(the bulk of a view's memory) and keeps the values; writes made meanwhile wait
in the mailbox. `UiSession::resume` rebuilds the tree from the assets and those
values, and refuses assets whose contract no longer matches.

Dropping the `UiSession` unmounts the view and ends it for Lua: its channel
stops delivering calls and `view:set` fails. Closing from Lua is visible
through `UiSession::is_closed`.

No display backend is connected yet. Views are laid out at
`DEFAULT_VIEWPORT`, and `UiSession::press_at` and `UiSession::texts` stand in
for touch input and rendering.
