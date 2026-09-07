# IMessage QQ Plugin

- Plugin ID: `imessage-qq`
- Direct Plugin dependencies: `imessage-gateway`, `webserver`, `captive-portal`
- Provided typed capabilities: none
- Storage: complete provider configuration under the Plugin-scoped KV key
  `configuration`

The Plugin starts without provider credentials when storage is empty. It requires the `IMessageGateway`
and `WebServer` capabilities, exposes `/api/gateway/qq` for runtime configuration,
and registers the configured QQ channel for its lifetime. Accepted configuration
is persisted before activation and restored during Plugin registration.
Malformed stored data fails registration. See [`http.md`](http.md).

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` with ID `imessage-qq`, title
`QQ`, module `entry.js`, and a `ResourceFiles` provider. Unload removes
the navigation entry and resource provider; no extra HTTP route is registered.

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

The page submits the existing POST configuration contract. It does not read
current settings or claim that acceptance verifies the upstream service. Secrets
are password inputs, never persisted in browser storage, and cleared on success
or unmount. Requests are cancelled on unmount and are never retried automatically.
The existing HTTP API has no authentication or transport encryption added here;
use only within a trusted provisioning network.
