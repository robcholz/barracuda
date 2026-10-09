# IMessage Inkbox Plugin

- Plugin ID: `imessage-inkbox`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Storage: complete provider configuration under the Plugin-scoped KV key
  `configuration`

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
connection; the Plugin owns no long-running task. The API key is never returned
or logged.

Inkbox shows an API key only once. When signup succeeds but the identity
lookup, storage, or Gateway registration fails, the endpoint state keeps that
signup result (API key, agent handle, mailbox, claim status, and a resolved
identity) in RAM, never in storage, until it is stored or the Plugin unloads.
A later signup for the same email resumes from it without a second upstream
signup; a signup for another email drops it.

The portal entry is registered with `CaptivePortal::register_with_status`; its
`GET /portal/status` record is `EntryStatus::configured`: `ready`
已配置/Configured while a channel is registered with the Gateway, otherwise
`off` 未配置/Not set up. It reads a flag kept beside the channel registration.

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
reads `GET /api/gateway/inkbox`: a stored signup resumes at the code step
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
