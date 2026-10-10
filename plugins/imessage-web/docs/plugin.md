# IMessage Web Plugin

- Plugin ID: `imessage-web`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: `IMessageWebRoute`

The Plugin requires the `IMessageGateway` capability and registers the `web`
message channel for its lifetime. It requires the `WebServer` capability,
mounts the WebSocket bridge, and publishes `IMessageWebRoute` for consumers of
the built-in Web conversation. Its portal entry registers no status source:
Web chat is built in rather than an external channel, so `imessage-web` is
absent from `GET /portal/status`.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`imessage-web`, group `WebGroup::Channel`, order 10, title `Web 聊天` / `Web
chat`, a bilingual summary, icon `icon.svg`, figure `figure.js`, module
`entry.js`, and a `ResourceFiles` provider. Unload removes the navigation entry
and resource provider; no extra HTTP route is registered.

Cargo automatically runs the declared `build` task before compiling this Plugin.
To build only its resources, run `cargo plugin run build --plugin imessage-web`
from the repository root. The declaration uses the shared `tools/web/build.ts`
script. Frontend dependencies and format/lint/check/test commands are managed
once at the repository root; tests belong to this Plugin's `resources/web/tests/`.
The independent browser module is
written to this Plugin's `filesystem/resources/entry.js`. The generic image
builder includes that directory only when this Plugin is selected. No business
page is bundled into the portal shell. The shared UI source is a build-time
helper, not a runtime dependency on another contributor's files.

The page (`resources/web/entry.ts`, built on the portal UI kit) connects to
WebSocket `/ws/message`, sends `WebClientFrame` JSON (`{ "text" }`; replies
exist only in the page, so a reply to an Agent message puts the quoted message
ahead of the user's words as `<quote>…</quote>` XML, escaped and cut to 512
UTF-8 bytes, inside `text`) and renders the
SSE-formatted frames the bridge returns: message start, delta, edit, delete,
reaction and end (an end error marks the message incomplete), the Agent's
semantic events inside `message.event` (reasoning, tool results, output and
effect output, turn and session errors; steps and token usage stay off the
page), permission requests (Allow and Deny send a text answer; any message
answers the pending request), typing, `stream.lagged`, and attachments, which
become downloads once complete (at most 8 MiB is kept per attachment). It
receives live events only, retains at most 100 log items with 65,536 UTF-16
code units per text part, and closes the socket when unmounted.

The page follows the design system's Chat card. A fresh temporary session
holds the composer mid-page under the `laptop` figure, with suggestions below;
once it has messages, a pinned head says 「临时会话」. The composer is one row:
the textarea fits its text up to 200px beside its send button. Messages are
capped at 1,024 UTF-8 bytes; past the cap send is disabled and one sentence
says so, with no counter. The device runs one turn at a time, so a message
written while a turn runs (or before the device starts the last one) waits in
the page's queue, where it can be edited or removed, and goes out when the turn
ends; a permission answer goes out at once. While a turn runs its author mark
spins (`resources/web/mark.ts`: WebGL, which plain HTTP allows; the static mark
and typing dots where WebGL is missing, the mark at rest under reduced motion)
and 「思考中」 shimmers until text streams. Replies stream in smoothly behind a
blinking caret; reasoning folds itself away as 「思考了 N 秒」; a running tool
spins; each finished reply ends with Copy and Reply icon buttons. The page's
own rules (`resources/web/style.ts`) are added on mount and removed on unmount.
Sends have no server receipt; a send counts as confirmed only when an Agent
message replies to it or the turn it answered goes on, and the disconnected
notice counts the rest; reconnection never resends, though queued messages that
never went out leave once the socket is back. The overview tile shows the
`laptop` figure (`resources/web/figure.js`); the sidebar icon
is `resources/web/icon.svg` (Lucide `message-square`).
