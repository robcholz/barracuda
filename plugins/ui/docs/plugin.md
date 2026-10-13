# UI Plugin

Runs compiled MicroTS views and shows them on the Board's primary display in
two layers, pressed by its touch input, and turns touch into the gestures the
system UI navigates by.

- Plugin ID: `ui`
- Dependencies: none
- Settings: none
- Context resources: peripheral slots `display.main` (`primary`) and
  `touch.main` (`primary`, optional)
- Provided capabilities: `UiService`
- Required capabilities: none
- Workflow: none
- Agent Tools: none
- Lua packages: none
- Web: none
- Tasks: one UI task that applies owners' writes to open views, draws the
  shown view, polls touch, and recognizes gestures
- Storage: none

The Plugin runs compiled MicroTS views. A view is a BVIR program
(`view.bvir`), its compiled stylesheet (`styles.bin`) and baked font atlases
(`font-<slot>.bin`), produced from MicroTS sources by the MicroTS compiler. The
crate `barracuda-ui-runtime` reads BVIR, mounts it into a vendored PocketJS
core tree, and re-evaluates the tree when values change.

`UiService::open` mounts an app's view on the app layer and
`UiService::open_on` a view on a chosen `Layer`; both return a `UiSession`,
kept by whoever presents the view. Its `ViewChannel` is the `Send + Sync` half for the code
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

## The display

The Plugin takes the Board's primary display through its `display.main` slot,
so a Board that enables it needs one, and lays every view out at the display's
size, one view pixel per display pixel.

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
`UiSession::texts` reads a view's text the same way.

Text without a size or weight class uses font slot 0 in the UI core, while the
PocketJS compiler means its 16 px default and bakes that as slot 2. When a
view ships slot 2 but no slot 0, the runtime serves slot 0 from it.

Without a display, as in tests or when no Board is selected, views are laid
out at `DEFAULT_VIEWPORT` and nothing is drawn.
