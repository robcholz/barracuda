# IMessage QQ Plugin

- Plugin ID: `imessage-qq`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Storage: complete provider configuration under the Plugin-scoped KV key
  `configuration`

The Plugin starts without provider credentials when storage is empty. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/qq` for runtime configuration,
and registers the configured QQ channel for its lifetime. See [`http.md`](http.md).

The stored configuration is `{"app_id","app_secret","api_base","token_url"}`.
QQ access tokens live at most two hours, so the Plugin stores the App Secret and
never a token. Before a new configuration is persisted, the endpoint exchanges
the App Secret for one access token at `token_url`; a rejection returns 422
`verification_failed` and stores nothing. Accepted configuration is persisted
before activation and restored during Plugin registration without any network
request.

The provider keeps the access token and its expiry instant in RAM only. It
fetches a token lazily before the first send and again once fewer than 60
seconds of its lifetime remain. When the QQ OpenAPI answers 401, it refreshes
the token once and retries the request once. Concurrent sends share one refresh.
OpenAPI requests carry `Authorization: QQBot <access_token>` and
`X-Union-Appid: <app_id>`.

A stored configuration in the former `access_token` shape does not block boot:
registration logs a warning and leaves the channel unconfigured until a new
configuration is saved. Other malformed stored data fails registration.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`imessage-qq`, group `WebGroup::Channel`, order 40, title `QQ`, a bilingual
summary, icon `icon.svg`, no figure, module `entry.js`, and a `ResourceFiles`
provider. Unload removes the navigation entry and resource provider; no extra
HTTP route is registered.

Cargo automatically runs the declared `build` task before compiling this Plugin.
To build only its resources, run `cargo plugin run build --plugin imessage-qq`
from the repository root. The declaration uses the shared `tools/web/build.ts`
script. Frontend dependencies and format/lint/check/test commands are managed
once at the repository root; tests belong to this Plugin's `resources/web/tests/`.
The independent browser module is
written to this Plugin's `filesystem/resources/entry.js`. The generic image
builder includes that directory only when this Plugin is selected. No business
page is bundled into the portal shell. The shared UI source is a build-time
helper, not a runtime dependency on another contributor's files.

The page (`resources/web/entry.ts`, built on the captive portal's UI kit) posts
`{app_id, app_secret, api_base, token_url}` to `POST /api/gateway/qq` with
「验证并保存」. The device fetches one access token before storing anything, so a 422
`verification_failed` puts QQ's own `message` and `code` on the App Secret
field; other errors toast the device's `message`. It does not read current
settings. Secrets are password inputs, never persisted in browser storage, and
cleared on success or unmount. Requests are cancelled on unmount and are never
retried automatically. The existing HTTP API has no authentication or transport
encryption added here; use only within a trusted provisioning network.
