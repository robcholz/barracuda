# IMessage QQ Plugin

- Plugin ID: `imessage-qq`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Gateway channel: `qq`
- Owned task: `qq_receive_task`, one receive loop (see below)
- Receive slots: one, while the channel is configured and in `send_receive`
- Storage (Plugin-scoped KV):
  - `configuration`: the provider configuration;
  - `mode`: the channel mode (`ChannelMode` JSON string);
  - `owners`: the allowed accounts (owner book);
  - `session`: the gateway session to resume, `{"session_id","seq"}`.

The Plugin starts without provider credentials when storage is empty. It requires the `IMessageGateway`
and `WebServer` capabilities and follows the channel plumbing of
[`imessage-gateway`](../../imessage-gateway/docs/plugin.md#how-a-channel-plugs-in):
`POST /api/gateway/qq` configures the channel and `GET /api/gateway/qq/status`
answers the shared channel status; `/api/gateway/qq/mode` and `/api/gateway/qq/owners` are the
shared mode and owner endpoints, served with it from one prefix route
(`ChannelEndpoint`). The allowed accounts are loaded only once the channel is
configured, and the gateway session by the first receive session. The `qq` channel is registered with the
Gateway exactly while it is configured and its mode is `send` or
`send_receive`. A configuration stored before modes existed loads as `send`;
a new configuration starts in `send_receive`. The portal entry's
`GET /portal/status` record is `entry_status` of the channel (未配置, 已停用,
仅发送, 收发中, 连接中, 名额已满, or 连接中断). See [`http.md`](http.md).

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

## Receiving

QQ delivers messages only over its WebSocket gateway, so the Plugin owns one
Embassy task, `qq_receive_task`, spawned in `start` and raced against the
Plugin's task token. It runs the channel crate's `receive_runtime`: parked
without a slot unless the channel is configured and in `send_receive`, it
leases one receive slot from `PluginContext::receive_slots` (never a request
pool connection) and runs one gateway session at a time on it.

A session:

1. takes the access token through the provider's cached path (a rejected App
   Secret halts receiving until the configuration changes);
2. asks `GET {api_base}/gateway` for the WebSocket URL once and caches it. QQ
   allows two such calls a minute, so a new lookup waits at least 30 s after
   the last one, and 60 s after QQ refused one. A refused upgrade drops the
   cached URL;
3. opens `wss://…` through the lease's `connect_stream` and `split`, and runs
   the `shared/ws-client` handshake (`Sec-WebSocket-Key` from the Platform
   entropy, `Sec-WebSocket-Accept` checked, masked frames);
4. waits for Hello (`op` 10) and sends Resume (`op` 6) when a session is
   stored, otherwise Identify (`op` 2) with intents `1<<25`
   (`GROUP_AND_C2C_EVENT`) and shard `[0, 1]`;
5. heartbeats (`op` 1 with the last `s`) every `heartbeat_interval`. The timer
   races the cancel-safe WebSocket read half, so a pending read never delays a
   heartbeat. A heartbeat still unacknowledged (`op` 11) when the next one is
   due ends the session, which then resumes;
6. reports `receiving` on `READY` (whose `session_id` is stored at once) or
   `RESUMED`;
7. while connected also refreshes the shared access token when it enters its
   last minute, for replies and the next login.

Each dispatch's `s` is kept in RAM and stored with the session id at most every
10 s. A close with code 4009, a server `op` 7, or a dropped connection resumes
at once after a healthy session. `op` 9 with `d:false` and every other close
code forget the session and identify again after the runtime's backoff; 4004
also renews the token. Close codes 4914 (delisted, sandbox only) and 4915
(banned) halt receiving with a bilingual message until the configuration
changes.

`C2C_MESSAGE_CREATE` becomes route `qq`/`c2c:<author.user_openid>` and
`GROUP_AT_MESSAGE_CREATE` route `qq`/`group:<group_openid>`, the same
conversation ids the send path takes. The sender checked against the owner book
is `author.user_openid` for direct messages and `author.member_openid` in
groups (QQ scopes both to the bot, so a person has a different id in each
scene). Text is trimmed; attachments, cards, and other non-text messages are
ignored. The 16 most recent message ids are remembered in RAM, so a message QQ
pushes again, also after a resume, is handled once. For each new message:

- an owner's message is published as `GatewayInboundMessage` with the QQ
  message `id` as `message_id`;
- a pairing code from a new sender adds them and answers 「已绑定，可以开始对话了」
  as a passive reply to the code message;
- anyone else is dropped and counted.

Before reading the next payload the session awaits `IMessageGateway::ready()`,
still heartbeating meanwhile.

### Passive replies

QQ accepts a reply to a received message for 60 minutes in a direct chat and 5
minutes in a group, and needs a distinct `msg_seq` for each reply to the same
message. The receive loop notes every received message with the provider; a
send whose `reply_to` names one carries `msg_id` and the next `msg_seq` (1, 2,
…) while its window is open and goes out as an ordinary message afterwards. A
`reply_to` the loop never saw (one received before a restart) is still sent
as a reply. Guild channels get `msg_id` without `msg_seq`. The provider keeps
the 16 most recent messages.

### Memory

Per receiving QQ channel: the receive slot's static socket buffers (1 KiB send,
4 KiB receive), one TLS session from the lease (about 35 KiB idle on device;
see the Platform manifests), the WebSocket read buffer (2 KiB idle; a larger
frame grows it up to its size, at most 64 KiB plus the header, and it shrinks
back once drained), a short-lived buffer per sent frame, the 2 KiB handshake
limit, about 1.5 KiB of recent message ids, the provider's reply book (16
entries), the cached gateway URL and session id, and the boxed session future.

## Configuration

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
field; other errors toast the device's `message`. On mount it reads `GET /api/gateway/qq/status`, of whose channel status it uses
`configured`; a configured channel shows as a 「通道」 row with the
已配置 card above the form that replaces it. The row also appears after a save
succeeds, and the page then calls `context.refreshStatus()` so the portal's
navigation and overview follow. It never reads settings or keys. Secrets are password inputs, never persisted in browser storage, and
cleared on success or unmount. Requests are cancelled on unmount and are never
retried automatically. The existing HTTP API has no authentication or transport
encryption added here; use only within a trusted provisioning network.
