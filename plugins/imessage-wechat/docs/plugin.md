# IMessage WeChat Plugin

- Plugin ID: `imessage-wechat`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Required typed capabilities: `IMessageGateway`, `WebServer`, `CaptivePortal`
- Workflow Actions, Events, and Agent Tools: none of its own; inbound text
  from allowed accounts is published through `IMessageGateway::publish` (the
  Gateway's `gateway.message.received` Event)
- Construction: `IMessageWechatPlugin::new(context)` keeps
  `context.http_clients`, `context.receive_slots`, and `context.entropy`
- Owned tasks: `wechat_login_task` and `wechat_receive_task` (pool size 1
  each), both started in `start` and raced against the `PluginTaskToken`
- Receive slots: one from `PluginContext::receive_slots` while the channel is
  linked and in `send_receive`; none otherwise
- Storage (Plugin-scoped KV keys):
  - `configuration`: the complete provider configuration;
  - `mode`: `"disabled"` or `"send_receive"` (JSON string);
  - `owners`: the allowed accounts (JSON array of `{id,label}`);
  - `get_updates_buf`: the last iLink `getupdates` cursor (raw bytes);
  - `context_tokens`: the latest `context_token` of up to 16 users (JSON
    array `[{user,token}]`, most recently used first);
  - `bot_id`: the `ilink_bot_id` of the last confirmed QR login.

The Plugin starts without provider credentials when storage is empty. It
requires the `IMessageGateway` and `WebServer` capabilities, exposes
`/api/gateway/wechat` for runtime configuration and status, and registers the
channel `wechat` while it is linked (a configuration is stored) and its mode
is not `disabled`. iLink cannot quote a message, so `reply_to` is ignored
instead of failing. Accepted configuration is persisted before activation and
restored during Plugin registration. Malformed stored configuration fails
registration. The allowed accounts are loaded only once a bot is linked. See
[`http.md`](http.md).

## Modes

WeChat offers `disabled` and `send_receive` (`ChannelMode::WITHOUT_SEND_ONLY`):
iLink sends need the inbound `context_token` and recent user activity, so
there is no send-only mode. A newly linked channel and a configuration stored
before modes existed both receive; a stored `send` is read as `send_receive`.
`disabled` drops the Gateway registration, the receive session, and the slot.

## Receiving

`wechat_receive_task` owns the shared receive runtime
(`barracuda-imessage-gateway-channel`). While the channel is linked and in
`send_receive` it holds one receive-slot lease, otherwise it is parked and
holds no connection. When every slot is in use the channel reports `no_slot`
and waits on `ReceiveSlots::wait_available`, so it starts as soon as another
channel frees one.

Each receive session opens one kept-alive connection to the configured
`api_base` through the lease's own client factory with the Gateway's poll
helper (`barracuda_imessage_gateway_plugin::poll`; not the `send` helper,
whose 30 s deadline is shorter than a long poll) and repeats:

1. await `IMessageGateway::ready()`;
2. `POST {api_base}/ilink/bot/getupdates` with
   `{"get_updates_buf":"<cursor>","base_info":{"channel_version":"barracuda-wechat"}}`
   and the headers `Content-Type`, `AuthorizationType: ilink_bot_token`,
   `Authorization: Bearer <token>`, `X-WECHAT-UIN` (base64 of the decimal
   string of a fresh random `u32` from the Platform entropy per request; the
   configured value when there is none), `iLink-App-Id`,
   `iLink-App-ClientVersion`, and `SKRouteTag` when configured;
3. bound the poll by iLink's `longpolling_timeout_ms` (35 s until a reply
   names one, kept within 5–120 s) plus 5 s. A poll that misses it is an
   empty success: the session reports `receiving` and reopens the connection
   at once;
4. for each new message: keep only `message_type` 1 (user) messages without a
   `group_id` whose `item_list` has text items (type 1); the bot's own
   (`message_type` 2) and non-text messages are skipped and counted. Message
   ids are `u64` on the wire and are kept as decimal strings;
5. classify the sender `from_user_id` (no label) with the owner book:
   - owner: publish `gateway.message.received` with route
     `{channel:"wechat", conversation_id:<from_user_id>}` (no `thread_id`),
     the message id, and the text;
   - pairing code: reply once with the paired message through the normal
     send path;
   - anyone else: drop and count;
6. store the latest `context_token` of an owner or a newly paired sender
   (strangers never take a slot), then the cursor.

A reply body over 32 KiB is not buffered whole: the poll keeps its first
32 KiB and last 4 KiB, takes `get_updates_buf` (and `longpolling_timeout_ms`)
from them, stores that cursor at once, and counts the batch as lost with a
warning, so the next poll moves past it. Only a reply whose cursor is in
neither part fails the poll.

A connection the server closes after a healthy poll is reopened at once.
Other failures go through the scaffold's backoff (1 s doubling to 30 s; 30 s
after HTTP 429). `ret` or `errcode` -14 means the bot session expired: the
loop halts with receive state `error` and message
「需要重新扫码 / Scan again to relink」 until a new QR login is confirmed or
the configuration or mode changes.

**Sending with the context token.** The provider attaches the latest stored
`context_token` of the recipient (`to_user_id`) to every send. A target
`thread_id` still overrides it. The token is never put in the route's
`thread_id`: it changes with every message and would split the route.

**Dedup and cursor.** The last 32 delivered message ids are kept in RAM, so a
batch redelivered after a reconnect or restart is published once. The cursor
moves after a batch is handled and is written when it changes in a batch
that had messages, otherwise at most every 5 minutes. A non-empty
`get_updates_buf` is the only one ever stored.

**Memory per receiving channel.** The lease's static 1 KiB TX and 4 KiB RX
socket buffers, one mbedTLS session over them while connected (record buffers
of about 2 × 16.7 KiB on the heap), a 2 KiB response-header buffer and a
512-byte read chunk per poll, the reply body (capped at 32 KiB, plus a 4 KiB
tail only while a larger batch is skipped), up to 32 remembered message ids, and up to 16 context tokens
(each user id ≤ 128 bytes and token ≤ 1 KiB).

**Login.** The QR login keeps its own task. A confirmed login stores the
configuration like a manual `POST`; when its `ilink_bot_id` differs from the
stored `bot_id`, the cursor and context tokens of the old bot are cleared.
Then receiving is synced to the mode and restarted, which also clears an
expired-session halt.

The portal entry is registered with `CaptivePortal::register_with_status`. Its
`GET /portal/status` record is `attention` 等待扫码/Waiting for scan while a QR
login session is in `wait` or `scanned`, `attention` 需要重新扫码/Scan again to
relink while receiving is halted by an expired bot session, and otherwise the
shared channel mapping (`entry_status`): `off` 未配置 when not linked, `off`
已停用 when disabled, `ready` 收发中 while receiving, `attention` 连接中,
名额已满 or 连接中断 otherwise.

The configuration can also be obtained by QR login at
`/api/gateway/wechat/login`. Registration retains two HTTP routes: the
configuration prefix, which also serves `/mode` and `/owners`
(`ChannelEndpoint`), and the exact login route. Both configuration paths
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
