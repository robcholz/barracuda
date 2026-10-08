# IMessage Telegram Plugin

- Plugin ID: `imessage-telegram`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Storage: complete provider configuration under the Plugin-scoped KV key
  `configuration`

The Plugin starts without provider credentials when storage is empty. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/telegram` for runtime configuration, and
registers the configured channel for its lifetime. Accepted configuration is
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
`imessage-telegram`, group `WebGroup::Channel`, order 20, title `Telegram`, a
bilingual summary, icon `icon.svg`, no figure, module `entry.js`, and a
`ResourceFiles` provider. Unload removes the navigation entry and resource
provider; no extra HTTP route is registered.

Cargo automatically runs the declared `build` task before compiling this Plugin.
To build only its resources, run `cargo plugin run build --plugin imessage-telegram`
from the repository root. The declaration uses the shared `tools/web/build.ts`
script. Frontend dependencies and format/lint/check/test commands are managed
once at the repository root; tests belong to this Plugin's `resources/web/tests/`.
The independent browser module is
written to this Plugin's `filesystem/resources/entry.js`. The generic image
builder includes that directory only when this Plugin is selected. No business
page is bundled into the portal shell. The shared UI source is a build-time
helper, not a runtime dependency on another contributor's files.

The page (`resources/web/entry.ts`, built on the captive portal's UI kit) posts
`{token, api_base, draft_min_delta_bytes}` to `POST /api/gateway/telegram`. Its
「验证」 button calls the Bot API's `getMe` straight from the browser (Telegram
allows any origin) at the 「高级」 `api_base`: a bot shows its name, `@username` and
a QR Code of `https://t.me/<username>`; Telegram's `error_code` and
`description` are shown on the token field. When the browser has no route out (a
phone on the device's hotspot) the page notes that the token is unverified;
verifying never gates saving. The device's error `message` is shown in the
toast. On mount it reads `GET /api/gateway/telegram`, which answers only
`{"configured"}`; a configured channel shows as a 「通道」 row with the
已配置 card above the form that replaces it. The row also appears after a save
succeeds, and the page then calls `context.refreshStatus()` so the portal's
navigation and overview follow. It never reads settings or keys. Secrets are password inputs, never
persisted in browser storage, and cleared on success or unmount. Requests are
cancelled on unmount and are never retried automatically. The existing HTTP API
has no authentication or transport encryption added here; use only within a
trusted provisioning network.
