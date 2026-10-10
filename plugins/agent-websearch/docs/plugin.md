# Agent Websearch Plugin

- Plugin ID: `agent-websearch`
- Direct Plugin dependencies: `agent`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Required typed capabilities: `AgentToolRegistry`, `WebServer`, `CaptivePortal`
- Owned tasks: none

During registration the Plugin adds the awaited `web_search` Tool directly to
`AgentToolRegistry`.

The Plugin owns Tavily configuration, outbound HTTP requests, provider response
validation, and Agent Tool response encoding. `POST /api/tavily` atomically
replaces the `api_base` and `api_key` entries in the Plugin's private KV scope,
then replaces the active in-memory configuration. The persisted configuration
is restored when the Plugin registers. An active search retains an `Rc`
snapshot of the configuration without cloning the API key. The API key is never
logged or returned.

`GET /api/tavily/status` reports the active configuration:
`{"configured":true,"config":{"api_base":"https://api.tavily.com"}}`, or
`{"configured":false}`. Other methods on it, and methods other than `POST` on
`/api/tavily`, answer `405`.

The portal entry is registered with `CaptivePortal::register_with_status`; its
`GET /portal/status` record is `EntryStatus::configured`: `ready`
已配置/Configured while a Tavily configuration is active, otherwise `off`
未配置/Not set up. It reads the shared in-memory configuration slot.

One reusable HTTP workspace prevents repeated allocation of header, read, and
request buffers. A concurrent search receives `busy`. The provider response is
bounded at 64 KiB.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`agent-websearch`, group `WebGroup::Agent`, order 20, title `网页搜索` / `Web
search`, a bilingual summary, icon `icon.svg`, figure `figure.js`, module
`entry.js`, and a `ResourceFiles` provider. Unload removes the navigation entry
and resource provider; the page uses the two `/api/tavily` routes above.

Cargo automatically runs the declared `build` task before compiling this Plugin.
To build only its resources, run `cargo plugin run build --plugin agent-websearch`
from the repository root. The declaration uses the shared `tools/web/build.ts`
script. Frontend dependencies and format/lint/check/test commands are managed
once at the repository root; tests belong to this Plugin's `resources/web/tests/`.
The independent browser module is
written to this Plugin's `filesystem/resources/entry.js`. The generic image
builder includes that directory only when this Plugin is selected. No business
page is bundled into the portal shell. The shared UI source is a build-time
helper, not a runtime dependency on another contributor's files.

The page is built on the portal UI kit: a header with the live `loupe` figure
(`resources/web/figure.js`, using the shell's global `HL`) and no title icon,
the Tavily API key, and an open 高级 fold with `api_base` (default
`https://api.tavily.com`). It renders zh or en from the portal language.
`resources/web/icon.svg` is the Lucide `search` mark.

The page submits the existing POST configuration contract and, once the device
accepts, calls `context.refreshStatus()` so the portal's navigation follows. Its
header shows the 状态 badge and the API base the device holds, read from
`GET /api/tavily/status` on mount and after each accepted save. It does not
claim that acceptance verifies the upstream service. Secrets
are password inputs, never persisted in browser storage, and cleared on success
or unmount. Requests are cancelled on unmount and are never retried automatically.
The existing HTTP API has no authentication or transport encryption added here;
use only within a trusted provisioning network.
