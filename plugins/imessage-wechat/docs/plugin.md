# IMessage WeChat Plugin

- Plugin ID: `imessage-wechat`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Required typed capabilities: `IMessageGateway`, `WebServer`, `CaptivePortal`
- Workflow Actions, Events, and Agent Tools: none
- Owned tasks: one `wechat_login_task` (pool size 1), started in `start` with a
  `PluginTaskToken`
- Storage: complete provider configuration under the Plugin-scoped KV key
  `configuration`

The Plugin starts without provider credentials when storage is empty. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/wechat` for runtime configuration, and
registers the configured channel `wechat` for its lifetime. iLink cannot quote a
message, so `reply_to` is ignored instead of failing; the target's `thread_id`
is sent as the iLink `context_token`. Accepted configuration is
persisted before activation and restored during Plugin registration. Malformed
stored data fails registration. `GET` on the same path reports only whether a
channel is configured. See [`http.md`](http.md).

The portal entry is registered with `CaptivePortal::register_with_status`. Its
`GET /portal/status` record is `attention` 等待扫码/Waiting for scan while a QR
login session is in `wait` or `scanned`; otherwise it is
`EntryStatus::configured`: `ready` 已配置/Configured while a channel is
registered with the Gateway, `off` 未配置/Not set up when none is. It reads the
login session's last status and a flag kept beside the channel registration.

The configuration can also be obtained by QR login at
`/api/gateway/wechat/login`. Registration retains both HTTP routes. Both routes
store and register through one shared path, so a confirmed login writes the
same `configuration` key and format as a manual `POST /api/gateway/wechat`:
the iLink `bot_token` as `token`, iLink's `baseurl` (or the `api_base` used for
the login) as `api_base`, and every other field at its default.

`wechat_login_task` owns the single login session. While no session runs it is
parked on a command signal and holds no iLink connection and no QR data. A
`POST` wakes it to fetch a QR code; it then long-polls iLink, one request at a
time, until the session is confirmed, expires, reaches 480 s, is cancelled by
`DELETE` or replaced by a new `POST`. Each session's state is boxed only while
the session runs; between sessions the task keeps only the last status. The
task races its loop with the `PluginTaskToken`, so unload, startup rollback,
and Plugin Manager teardown drop the running iLink request and release any
waiting `POST` with `502`.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`imessage-wechat`, group `WebGroup::Channel`, order 30, title `微信` / `WeChat`, a
bilingual summary, icon `icon.svg`, no figure, module `entry.js`, and a
`ResourceFiles` provider. Unload removes the navigation entry and resource
provider; no extra HTTP route is registered.

Cargo automatically runs the declared `build` task before compiling this Plugin.
To build only its resources, run `cargo plugin run build --plugin imessage-wechat`
from the repository root. The declaration uses the shared `tools/web/build.ts`
script. Frontend dependencies and format/lint/check/test commands are managed
once at the repository root; tests belong to this Plugin's `resources/web/tests/`.
The independent browser module is
written to this Plugin's `filesystem/resources/entry.js`. The generic image
builder includes that directory only when this Plugin is selected. No business
page is bundled into the portal shell. The shared UI source is a build-time
helper, not a runtime dependency on another contributor's files.

The page (`resources/web/entry.ts`, built on the captive portal's UI kit) starts
a QR login on mount (`POST /api/gateway/wechat/login`), draws the returned `url`
as a QR Code with a countdown from `expires_in`, polls `GET` every 2 s (wait,
scanned, confirmed, expired or failed) and sends a `keepalive` `DELETE` when it
goes away. A `GET` that reports `configured` with no session running shows the
linked state with 「重新绑定」 instead of starting a login. On a phone (narrower than
720 px) it shows the steps and a 「复制链接」 button instead of the code and starts no
session. A token entered by hand under 「高级」 posts to `POST /api/gateway/wechat`;
the footer shows only while that fold is open. After a confirmed login or an
accepted token the page calls `context.refreshStatus()`, so the portal's
navigation and overview follow. Secrets are password inputs,
never persisted in browser storage, and cleared on success or unmount. Requests
are cancelled on unmount and are never retried automatically. The existing HTTP
API has no authentication or transport encryption added here; use only within a
trusted provisioning network.
