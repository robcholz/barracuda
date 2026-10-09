# IMessage Telegram Plugin

- Plugin ID: `imessage-telegram`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Required typed capabilities: `IMessageGateway`, `WebServer`, `CaptivePortal`
- Workflow Actions, Events, Agent Tools: none (inbound messages reach Workflow
  as the Gateway's `gateway.message.received`)
- Owned tasks: one, `telegram_receive_task`, which owns the receive loop
- Receive slots: at most one, held only while the channel is configured and in
  `send_receive`
- Storage (Plugin-scoped KV):
  - `configuration`: the complete provider configuration;
  - `mode`: the channel mode as a JSON string (see below);
  - `owners`: the allowed accounts, a JSON array of `{id, label}`;
  - `offset`: the receive cursor, the next `getUpdates` offset as a decimal
    string.

The Plugin starts without provider credentials when storage is empty. Its
constructor takes the Platform HTTP clients, the receive slots, and the
Platform entropy (for binding codes) from `PluginContext`. Registration loads
the configuration and mode, and the allowed accounts only when configured;
malformed stored data fails registration. The first receive session reads the
cursor. The configuration path, `/mode`, and `/owners` are one prefix route
(`ChannelEndpoint`). See [`http.md`](http.md) for the endpoints.

## Modes and registration

The mode is a `ChannelMode` from `barracuda-imessage-gateway-channel`:

- `disabled`: no Gateway channel, no connection;
- `send`: the `telegram` channel is registered with the Gateway and only sends;
- `send_receive`: registered, plus the receive loop.

A newly configured channel starts in `send_receive`. A configuration stored
before modes existed loads as `send`. The `telegram` channel is registered with
the Gateway exactly while the channel is configured and its mode is not
`disabled`; the registration lives in the channel state the endpoints and the
task share.

## Receive loop

`telegram_receive_task` is spawned once in `start` and races the receive
runtime (`receive_runtime` from the channel crate) against its
`PluginTaskToken`. While the channel is not configured or not in
`send_receive`, the runtime is parked and holds no slot. Otherwise it holds one
lease from the System's receive slots (never the shared request pool used by
model calls and sends); when every slot is taken it reports `no_slot` and
starts as soon as `ReceiveSlots::wait_available` reports a freed slot.

Each session opens one kept-alive connection on the lease's client factory,
through the Gateway's poll helper (`barracuda_imessage_gateway_plugin::poll`),
and loops:

1. await `IMessageGateway::ready()`;
2. `GET /bot<token>/getUpdates?timeout=50&limit=8&allowed_updates=["message"]&offset=<cursor>`;
3. on an `ok` answer, report `receiving` and handle each update in order.

A connection that ends after a healthy poll is reopened at once; other
failures back off from 1 s to 30 s.

- **Cursor.** The offset (last `update_id` + 1) acknowledges every handled
  update on the next poll. It lives in RAM and is written to `offset` at most
  once every 10 s. A new bot token clears it.
- **Duplicates.** The last 16 handled `update_id`s are remembered; an update
  sent again, for example after a reconnect, is dropped.
- **Text only.** Only `message.text` with a sender is handled; media, stickers,
  service messages and anything else are skipped with a debug log.
- **Allowed accounts.** The sender is `from.id`, labelled `@username` or the
  first name. `Owners::classify` decides:
  - an owner's message is published as a `GatewayInboundMessage` with route
    `{channel: "telegram", conversation_id: <chat.id>}` and `message_id`
    `<message_id>`, so replies (and `reply_to`) land in the same chat;
  - a new sender whose text is the binding code, or `/start <code>`, becomes an
    owner and gets `PAIRED_REPLY` through the provider's ordinary
    `sendMessage`;
  - anyone else is dropped and counted.
- **Errors.**
  - 409 because a webhook is set: `deleteWebhook` is called once per entry into
    `send_receive` (or new configuration), then polling resumes. Another 409,
    including 「terminated by other getUpdates request」 from a second poller,
    reports `error` with Telegram's description and backs off.
  - 429: waits at least `parameters.retry_after`.
  - 401 or 404 (a wrong or revoked token): reports `error` and stops until the
    configuration or mode changes.
- **Memory.** Per receiving channel:
  - a 1 KiB response header buffer and a 512-byte read chunk;
  - one response body of at most 16 KiB while a poll is decoded; it decodes
    straight into the few fields used. A batch over the cap is fetched again
    one update at a time, and a single update over it is acknowledged and
    skipped with a warning;
  - the lease's TLS session, about 35 KiB idle with the 16 KiB input and 4 KiB
    output record buffers;
  - the slot's static 1 KiB transmit and 4 KiB receive socket buffers.

Deadlines: 30 s to connect, 70 s per poll, 30 s for `deleteWebhook`.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`imessage-telegram`, group `WebGroup::Channel`, order 20, title `Telegram`, a
bilingual summary, icon `icon.svg`, no figure, module `entry.js`, and a
`ResourceFiles` provider. Its `GET /portal/status` record is the channel
crate's `entry_status`: 未配置/Not set up, 已停用/Disabled, 仅发送/Send only,
收发中/Receiving, 连接中/Connecting, 名额已满/No slot, or 连接中断/Disconnected.
Unload removes the navigation entry and resource provider.

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
toast. On mount it reads `GET /api/gateway/telegram`, which reports the
channel status and never the settings; a configured channel shows as a 「通道」
row with the 已配置 card above the form that replaces it. The row also appears
after a save succeeds, and the page then calls `context.refreshStatus()` so the
portal's navigation and overview follow. It never reads settings or keys.
Secrets are password inputs, never persisted in browser storage, and cleared on
success or unmount. Requests are cancelled on unmount and are never retried
automatically. The existing HTTP API has no authentication or transport
encryption added here; use only within a trusted provisioning network.
