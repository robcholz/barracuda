# IMessage BlueBubble Plugin

- Plugin ID: `imessage-bluebubble`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Storage: complete provider configuration under the Plugin-scoped KV key
  `configuration`

The Plugin starts without provider credentials when storage is empty. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/bluebubbles` for runtime configuration, and
registers the configured channel `bluebubbles` for its lifetime, so it can be
active beside the Inkbox `inkbox` channel. With the Private API enabled,
`reply_to` is sent as `selectedMessageGuid`; with it disabled, AppleScript sends
cannot quote a message, so `reply_to` is ignored and the text or attachment is
sent unthreaded instead of failing. Accepted configuration is
persisted before activation and restored during Plugin registration. Malformed
stored data fails registration. `GET` on the same path reports only whether a
channel is configured. The portal entry is registered with
`CaptivePortal::register_with_status`; its `GET /portal/status` record is
`EntryStatus::configured`: `ready` 已配置/Configured while a channel is
registered with the Gateway, otherwise `off` 未配置/Not set up. It reads a flag
the endpoint keeps beside its registration. See [`http.md`](http.md).

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`imessage-bluebubble`, group `WebGroup::Channel`, order 50, title `BlueBubbles`,
a bilingual summary, icon `icon.svg`, no figure, module `entry.js`, and a
`ResourceFiles` provider. Unload removes the navigation entry and resource
provider; no extra HTTP route is registered.

Cargo automatically runs the declared `build` task before compiling this Plugin.
To build only its resources, run `cargo plugin run build --plugin imessage-bluebubble`
from the repository root. The declaration uses the shared `tools/web/build.ts`
script. Frontend dependencies and format/lint/check/test commands are managed
once at the repository root; tests belong to this Plugin's `resources/web/tests/`.
The independent browser module is
written to this Plugin's `filesystem/resources/entry.js`. The generic image
builder includes that directory only when this Plugin is selected. No business
page is bundled into the portal shell. The shared UI source is a build-time
helper, not a runtime dependency on another contributor's files.

The page (`resources/web/entry.ts`, built on the captive portal's UI kit) posts
`{server_url, password, use_private_api, stream_edit_min_delta_bytes,
stream_max_edits}` to `POST /api/gateway/bluebubbles`. Its 「测试连接」 button calls
the server's `GET /api/v1/server/info?password=…` straight from the browser: the
server version, macOS version and Private API state are shown, and the Private
API switch is set to the server's value; the server's refusal is shown on the
URL field. When the browser cannot reach the server (no route or CORS) the page
notes that the connection is untested; testing never gates saving.
On mount it reads `GET /api/gateway/bluebubbles`, which answers only
`{"configured"}`; a configured channel shows as a 「通道」 row with the
已配置 card above the form that replaces it. The row also appears after a save
succeeds, and the page then calls `context.refreshStatus()` so the portal's
navigation and overview follow. It never reads settings or keys. Secrets are password inputs, never persisted in browser
storage, and cleared on success or unmount. Requests are cancelled on unmount
and are never retried automatically. The existing HTTP API has no authentication
or transport encryption added here; use only within a trusted provisioning
network.
