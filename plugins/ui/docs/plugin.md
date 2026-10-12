# UI Plugin

Runs compiled MicroTS views and lays them out for whoever owns them.

- Plugin ID: `ui`
- Dependencies: none
- Settings: none
- Context resources: none
- Provided capabilities: `UiService`
- Required capabilities: none
- Workflow: none
- Agent Tools: none
- Lua packages: none
- Web: none
- Tasks: one UI task that applies owners' writes to open views
- Storage: none

The Plugin runs compiled MicroTS views. A view is a BVIR program
(`view.bvir`), its compiled stylesheet (`styles.bin`) and baked font atlases
(`font-<slot>.bin`), produced from MicroTS sources by the MicroTS compiler. The
crate `barracuda-ui-runtime` reads BVIR, mounts it into a vendored PocketJS
core tree, and re-evaluates the tree when values change.

`UiService::open` mounts a view and returns a `UiSession`, kept by whoever
presents the view. Its `ViewChannel` is the `Send + Sync` half for the code
that drives the view, on any task: `ViewChannel::set` writes a `ViewValue`,
and `ViewChannel::next` waits for the next handler call, whose arguments are
`ViewValue`s too. This Plugin knows nothing about Lua; a Plugin that gives a
view to Lua converts the values itself.

Writes are checked against the view contract (`ViewChannel::value_type`):
an undeclared name fails with `ViewError::UnknownValue` and a value of another
type with `ViewError::Mismatch`, and neither reaches the tree. Writes keep only
the latest value per slot, and arrays hold at most `MAX_ARRAY_ITEMS` items.
Handler calls queue up to `MAX_EVENTS` per view, and `UiSession::clear_events`
drops the ones not yet received when the view stops taking input.

The owner never touches the UI tree. It shares only a critical-section mailbox
with the UI task, which wakes on writes, applies them, and lays out every
changed view.

A view is a function of its values. `UiSession::suspend` releases the tree
(the bulk of a view's memory) and keeps the values; writes made meanwhile wait
in the mailbox. `UiSession::resume` rebuilds the tree from the assets and those
values, and refuses assets whose contract no longer matches.

Dropping the `UiSession` unmounts the view and ends it for the channel: it
stops delivering calls and writes fail with `ViewError::Closed`. Closing
through the channel is visible through `UiSession::is_closed`.

No display backend is connected yet. Views are laid out at
`DEFAULT_VIEWPORT`, and `UiSession::press_at` and `UiSession::texts` stand in
for touch input and rendering.
