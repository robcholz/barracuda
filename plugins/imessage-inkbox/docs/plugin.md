# IMessage Inkbox Plugin

- Plugin ID: `imessage-inkbox`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Gateway channel: `inkbox`
- Owned task: `inkbox_receive_task`, one receive loop (see below)
- Receive slots: one, while the channel is configured and in `send_receive`
- Storage (Plugin-scoped KV):
  - `configuration`: the provider configuration;
  - `mode`: the channel mode (`ChannelMode` JSON string);
  - `owners`: the allowed accounts (owner book);
  - `cursor`: the receive cursor, `{"start":"<created_at>","recent":["<id>",…]}`.

The Plugin starts without provider credentials when storage is empty. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/inkbox` for runtime configuration, and
registers the configured channel `inkbox` for its lifetime, so it can be active
beside the BlueBubbles `bluebubbles` channel. Inkbox cannot quote a message, so
`reply_to` on text and media is ignored instead of failing. Accepted configuration is
persisted before activation and restored during Plugin registration. Malformed
stored data fails registration. `GET /api/gateway/inkbox` reports whether a
channel is configured and, for a signup, the person's address, the agent
mailbox and the claim status. See
[`http.md`](http.md).

The configuration is `{"api_key","identity_id","api_base"}`, plus
`"signup":{"human_email","email_address","claim_status"}` when it came from
signup. It is
written either by `POST /api/gateway/inkbox` with an existing API key (no
`signup`), or by Inkbox agent self-signup: `POST /api/gateway/inkbox/signup`
creates an agent for a person's email address, resolves the returned agent
handle to its identity UUID, stores the configuration with the production
`api_base`, and registers the channel. `POST /api/gateway/inkbox/verify` and
`POST /api/gateway/inkbox/resend` then use the stored API key to submit or
resend the emailed code; a successful verify stores the new claim status. The
Plugin retains one `WebRouteRegistration` per path. Signup, verify, and resend
run inside their HTTP request, one at a time, with at most one upstream
connection. The API key is never returned or logged.

Inkbox shows an API key only once. When signup succeeds but the identity
lookup, storage, or Gateway registration fails, the endpoint state keeps that
signup result (API key, agent handle, mailbox, claim status, and a resolved
identity) in RAM, never in storage, until it is stored or the Plugin unloads.
A later signup for the same email resumes from it without a second upstream
signup; a signup for another email drops it.

The Plugin follows the channel plumbing of
[`imessage-gateway`](../../imessage-gateway/docs/plugin.md#how-a-channel-plugs-in):
`/api/gateway/inkbox/mode` and `/api/gateway/inkbox/owners` are the shared mode
and owner endpoints, served with `/api/gateway/inkbox` from one prefix route
(`ChannelEndpoint`); the allowed accounts are loaded only once the channel is
configured, and the cursor by the first receive session. The `inkbox` channel is registered with the Gateway
exactly while it is configured and its mode is `send` or `send_receive`. A
configuration stored before modes existed loads as `send`. A device without an
Inkbox channel starts in `send_receive`, and a signup on such a device sets
`send_receive` before storing its result. The portal entry's `GET
/portal/status` record is `entry_status` of the channel (未配置, 已停用, 仅发送,
收发中, 连接中, 名额已满, or 连接中断).

## Receiving

Inkbox offers no long poll, WebSocket, or push a device on a private network
can receive, so the Plugin polls. It owns one Embassy task,
`inkbox_receive_task`, spawned in `start` and raced against the Plugin's task
token, running the channel crate's `receive_runtime`. Parked without a slot
unless the channel is configured and in `send_receive`, it leases one receive
slot (never a request pool connection) and opens one kept-alive connection on
it with `client_factory()`. Every 3 s it sends

```
GET {api_base}/api/v1/imessage/messages?limit=50&start_datetime=<cursor>
X-API-Key: <api key>
```

after awaiting `IMessageGateway::ready()`. The list is newest first and
`start_datetime` is inclusive.

- **Cursor.** `start` is the `created_at` of the newest message seen, sent
  percent-encoded. The 32 most recent ids handled are kept beside it and
  dropped when the inclusive start repeats them (also across reconnects and
  restarts). The cursor is stored at most every 10 s, and at once after the
  first poll.
- **First poll.** Without a cursor (a new identity), the poll asks for one
  message and only records where to start, so earlier messages are not
  replayed to the agent. An empty inbox starts at the epoch.
- **Messages.** Each page is handled oldest first. Only `direction` `inbound`
  messages with text are handled; the sender is `remote_number` and the route
  is `inkbox`/`conversation_id`, the conversation the send path takes. An
  owner's message is published with the Inkbox message `id`; a pairing code
  from a new sender adds them and sends 「已绑定，可以开始对话了」 to that
  conversation through the normal send path; anyone else is dropped and
  counted. Group messages are not listed (Inkbox's `include_groups` stays off).
- **Large pages.** A body over 32 KiB is asked for again with half the limit;
  more than a full page of new messages within one interval skips the oldest,
  with a warning.
- **Errors.** 401 or 403 halts receiving with
  「Inkbox 拒绝了 API Key / Inkbox rejected the API key」 until the
  configuration changes; 429 waits `Retry-After` seconds (60 when absent);
  other statuses and failed connections retry with the runtime's backoff; a
  connection that ends after a healthy poll is reopened at once.

Mail is not polled: a signup's agent mailbox receives mail at Inkbox, but only
iMessage reaches the Gateway.

Memory per receiving Inkbox channel: the receive slot's static socket buffers
(1 KiB send, 4 KiB receive), one TLS session from the lease (about 35 KiB idle
on device; see the Platform manifests), a 2 KiB response-head buffer, a
response body of at most 32 KiB while one poll is parsed (usually one or two
messages, under 2 KiB), the parsed page, and the cursor (up to 32 ids, about
2 KiB).

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`imessage-inkbox`, group `WebGroup::Channel`, order 60, title `Inkbox`, a
bilingual summary, icon `icon.png`, no figure, module `entry.js`, and a
`ResourceFiles` provider. Unload removes the navigation entry and resource
provider; no extra HTTP route is registered.

Cargo automatically runs the declared `build` task before compiling this Plugin.
To build only its resources, run `cargo plugin run build --plugin imessage-inkbox`
from the repository root. The declaration uses the shared `tools/web/build.ts`
script. Frontend dependencies and format/lint/check/test commands are managed
once at the repository root; tests belong to this Plugin's `resources/web/tests/`.
The independent browser module is
written to this Plugin's `filesystem/resources/entry.js`. The generic image
builder includes that directory only when this Plugin is selected. No business
page is bundled into the portal shell. The shared UI source is a build-time
helper, not a runtime dependency on another contributor's files.

The page (`resources/web/entry.ts`, built on the captive portal's UI kit) offers
two methods. 「用邮箱新建」 posts the email to `POST /api/gateway/inkbox/signup`, then
the emailed 6-digit code to `/verify` (「重新发送」 calls `/resend`; a cooldown is
shown beside it) and shows the claimed identity's address; Inkbox's own messages
are shown on the field they concern, and a 409 `conflict` returns to the email
step. A signup error with `"retry": true` (the device kept the signup) offers
「重试」, which posts the same email again and resumes it without a second email.
「已有 API Key」 posts `{api_key, identity_id, api_base}` to `POST
/api/gateway/inkbox`; the form footer shows only in this mode. On mount the page
reads `GET /api/gateway/inkbox` (the channel status plus `signup`): a stored signup resumes at the code step
(`claim_status` other than `agent_claimed`; the note names `human_email` and
prefills the email field, or says the code went to the person's inbox when
`human_email` is absent) or at the claimed
step with the agent's `email_address`; a channel configured with a key shows a
「通道」 row with the 已配置 card above the form. After a signup, a claim or a saved
key the page calls `context.refreshStatus()`. It never reads settings or keys.
Secrets are password inputs, never persisted in browser
storage, and cleared on success or unmount. Requests are cancelled on unmount
and are never retried automatically. The existing HTTP API has no authentication
or transport encryption added here; use only within a trusted provisioning
network.
