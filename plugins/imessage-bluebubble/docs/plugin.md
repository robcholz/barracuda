# IMessage BlueBubble Plugin

- Plugin ID: `imessage-bluebubble`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Required capabilities: `IMessageGateway`, `WebServer`, `CaptivePortal`; the
  Platform entropy (`PluginContext::entropy`) and IP stack
  (`PluginContext::ip_stack`)
- Owned task: `bluebubbles_receive_task`, one receive runtime (below)
- Receive slots: none. The webhook is served by the device's own webserver, so
  the channel uses `UnlimitedSlots` and never reports `no_slot`.
- Storage (Plugin-scoped KV):
  - `configuration`: the provider configuration;
  - `mode`: the channel mode;
  - `owners`: the allowed accounts;
  - `webhook`: `{"server","secret","url"?}`, the webhook secret, the server it
    was minted for, and the URL last registered;
  - `cursor`: `{"after"?,"seen":[…]}`, the catch-up cursor.

The Plugin starts without provider credentials when storage is empty. It
exposes `/api/gateway/bluebubbles` for runtime configuration and registers the
channel `bluebubbles` while it is configured and its mode is `send` or
`send_receive`, so it can be active beside the Inkbox `inkbox` channel. With the
Private API enabled, `reply_to` is sent as `selectedMessageGuid`; with it
disabled, AppleScript sends cannot quote a message, so `reply_to` is ignored and
the text or attachment is sent unthreaded instead of failing. Accepted
configuration is persisted before activation and restored during Plugin
registration. Malformed stored data fails registration. See
[`http.md`](http.md).

## Modes

The channel follows the shared plumbing in the Gateway's `docs/plugin.md`
("How a channel plugs in").

- `disabled`: not registered with the Gateway; no receiving.
- `send`: registered and sends only. Configurations stored before modes
  existed load as `send`.
- `send_receive`: registered, plus webhook receiving. This is the default for a
  new channel.

Leaving `send_receive` stops the receive session and then deletes the webhook
from the server, waiting at most 10 s. A failure is logged and keeps the
record, so the next registration deletes the stale entry. The portal status
comes from `entry_status`: 仅发送, 收发中, 连接中, 连接中断 or 已停用.

## Receiving

Receiving is a LAN webhook. The BlueBubbles server posts `new-message` events
to `POST /api/gateway/bluebubbles/hook/<secret>` on the device's webserver
(port 8787), so it uses no outbound connection and no receive slot.

**Secret.** The webserver exposes no headers or query, so the path carries the
secret. It is 128 bits from the Platform entropy, written as 32 hex digits, and
compared in constant time. It is stored in `webhook` with the server URL it was
minted for. A new server URL mints a new secret and starts a new cursor.
Without entropy, receiving halts with an error.

**Endpoint.** The route is registered as a prefix
(`serve_http_prefix("/api/gateway/bluebubbles/hook")`), because the secret is
not known at registration. The handler only checks and queues; it never
awaits.
- It answers 404 to a wrong secret or while not in `send_receive`, and 405 to a
  method other than `POST`.
- Otherwise it answers 204 at once and does one of these:
  - drops a body over 8 KiB, counts it as lost, and asks for a catch-up;
  - ignores other event types, unparsable bodies, and `isFromMe` messages;
  - drops a GUID that is already handled or queued;
  - queues the message.
- The inbox holds at most 8 messages and 16 KiB of text. A delivery that does
  not fit is counted as lost and asks for a catch-up.

Only `type`, `data.guid`, `data.text`, `data.isFromMe`, `data.handle.address`,
`data.chats[0].guid`, `data.dateCreated`, `data.itemType` and
`data.associatedMessageType` are parsed. Everything else is skipped by serde
without building a JSON tree.

**Receive task.** `bluebubbles_receive_task` is spawned once in `start`. It
races the runtime from `receive_runtime(.., ReceiveTiming::DEVICE)` against
the Plugin's task token. Each session does the following:

1. It takes the secret, minting it if needed. It builds the URL
   `http://<station IPv4>:8787/api/gateway/bluebubbles/hook/<secret>` from
   `ip_stack.config_v4()`. Without an address the session fails with "device
   LAN address unknown" and is retried with backoff.
2. When no cursor exists, it starts the cursor at the newest message
   (`message/query` `sort: DESC, limit: 1`), so a new channel does not replay
   history.
3. It registers the webhook. It lists `GET /api/v1/webhook`, deletes this
   device's stale entries (the stored URL, or any URL ending in this secret's
   path), and creates `{"url":…,"events":["new-message"]}` when the current
   URL is missing. Another device's entries are left alone. Then it reports
   `receiving`.
4. It catches up with `POST /api/v1/message/query`
   `{"after":<cursor>,"sort":"ASC","with":["chat"],"limit":10,"offset":n}`.
   - Pages hold 10 messages. The limit is halved while a page exceeds 16 KiB.
     A single message over 16 KiB is skipped and counted as lost.
   - One catch-up handles at most 50 messages. When `metadata.total` reports
     more, the oldest are skipped and counted as lost.
   - Before each page it awaits `IMessageGateway::ready()`.
5. It loops over three events:
   - Queued messages: it awaits `ready()` and handles each one.
   - A lost delivery: it catches up.
   - Every 3 minutes: it registers the webhook again, re-creating it if the
     server dropped it, and catches up. The server never retries a delivery,
     and the webserver refuses bodies over its 8 KiB request buffer, so this
     recovers both.

Handling a message:
- It records the GUID and advances the cursor.
- It skips `isFromMe`, and counts as skipped anything that is not plain text:
  attachments (`U+FFFC`), tapbacks, group events, and messages without a
  sender or chat.
- It then calls `owners.classify(handle.address, None, text)`:
  - `Owner`: publishes `GatewayInboundMessage` with route
    `{channel:"bluebubbles", conversation_id:<chats[0].guid>}` and
    `message_id` set to the message GUID. The chat GUID is the send path's
    `chatGuid`, so replies land in the same chat.
  - `Paired`: sends `PAIRED_REPLY` to that chat through the provider.
  - `Ignored`: drops the message; the owner book counts it.

Server errors map to the runtime's policy:
- 401/403 halts with "BlueBubbles rejected the password" until the
  configuration changes.
- 429 retries after at least 30 s.
- Anything else retries with backoff.

**Cursor and dedup.** `after` is the newest `dateCreated` handled. `seen`
holds the 32 newest GUIDs, because `after` is inclusive. Both live in RAM. The
receive task stores them at most every 10 s while they change, so a reboot can
replay at most the last 10 s of messages, and the GUID ring catches those
that are still in it. The webhook and the catch-up dedup against the ring and
the inbox.

**Memory.**
- The inbox holds at most 8 messages and 16 KiB of text.
- The ring holds 32 GUIDs, about 1.3 KiB.
- A catch-up page holds at most 16 KiB of body, plus the Gateway HTTP
  helper's 16 KiB header and 8 KiB read buffers, all in bulk memory, only
  while a request runs. A webhook list holds at most 8 KiB.
- An `http://` server URL needs no TLS. An `https://` URL holds one TLS session
  per request from the shared request pool, never a receive lease.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`imessage-bluebubble`, group `WebGroup::Channel`, order 50, title `BlueBubbles`,
a bilingual summary, icon `icon.svg`, no figure, module `entry.js`, and a
`ResourceFiles` provider whose status is `entry_status` of the channel. Unload
removes the navigation entry, the resource provider, and the two HTTP routes:
the configuration prefix, which also serves `/mode` and `/owners`
(`ChannelEndpoint`), and the `/hook` prefix.

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
On mount it reads `GET /api/gateway/bluebubbles/status` (the shared channel status);
a configured channel shows as a 「通道」 row with the 已配置 card above the form
that replaces it, and the shared mode and allowed-accounts controls
(`channelInbound`) use `/mode` and `/owners`. The row also appears after a save
succeeds, and the page then calls `context.refreshStatus()` so the portal's
navigation and overview follow. It never reads settings or keys. Secrets are password inputs, never persisted in browser
storage, and cleared on success or unmount. Requests are cancelled on unmount
and are never retried automatically. The existing HTTP API has no authentication
or transport encryption added here; use only within a trusted provisioning
network.
