# Agent Websearch Plugin

- Plugin ID: `agent-websearch`
- Direct Plugin dependencies: `agent`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Required typed capabilities: `AgentToolRegistry`, `WebServer`
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

One reusable HTTP workspace prevents repeated allocation of header, read, and
request buffers. A concurrent search receives `busy`. The provider response is
bounded at 64 KiB.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` with ID `agent-websearch`, title
`网页搜索`, module `entry.js`, and a `ResourceFiles` provider. Unload removes
the navigation entry and resource provider; no extra HTTP route is registered.

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

The page submits the existing POST configuration contract. It does not read
current settings or claim that acceptance verifies the upstream service. Secrets
are password inputs, never persisted in browser storage, and cleared on success
or unmount. Requests are cancelled on unmount and are never retried automatically.
The existing HTTP API has no authentication or transport encryption added here;
use only within a trusted provisioning network.
