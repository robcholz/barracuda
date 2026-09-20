# IMessage Web Plugin

- Plugin ID: `imessage-web`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: `IMessageWebRoute`

The Plugin requires the `IMessageGateway` capability and registers the `web`
message channel for its lifetime. It requires the `WebServer` capability,
mounts the WebSocket bridge, and publishes `IMessageWebRoute` for consumers of
the built-in Web conversation.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` with ID `imessage-web`, title
`Web 聊天`, module `entry.js`, and a `ResourceFiles` provider. Unload removes
the navigation entry and resource provider; no extra HTTP route is registered.

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

The page connects to WebSocket `/ws/message`, sends `WebClientFrame` JSON, and renders the
SSE-formatted text events returned by the bridge, including `output_delta`.
It supports text messages, not attachment playback or upload. It receives live
events only, retains at most 100 visible messages with 65,536 UTF-16 code units per remote
message, and closes the socket when unmounted. Sends have no server receipt;
reconnection never resends.
