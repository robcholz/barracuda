# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: `webserver`, `workflow`, `captive-portal`
- Provided typed capabilities: `AgentRuntime`, `AgentToolRegistry`
- Required typed capabilities: `WebServer` from `webserver`,
  `WorkflowActionRegistry` and `WorkflowService` from `workflow`, and
  `CaptivePortal` from `captive-portal`
- Filesystem: private; reads bundled `/resources/workflows.json`
- Owned Components: none
- Plugin-owned tasks: Agent runtime and `session.event` forwarding

System constructs the Agent Plugin from the common `PluginContext`. During
registration it creates and publishes `AgentRuntime`, publishes
`AgentToolRegistry`, and registers Agent session operations as direct Workflow
Actions. The Workflow Plugin remains independent of Agent; Agent is the
capability consumer.

Direct dependent Plugins use `AgentRuntime` for typed session collaboration.
Transport- or environment-specific request shaping stays in those adapter
Plugins rather than in the Agent runtime.

The Plugin owns and drives `RuntimeService` directly. Alongside it, the
Agent-to-Workflow adapter only forwards open-session output as `session.event`;
it does not own or run either the Agent runtime or the Workflow runtime.

Dependent Plugins may still register native Agent `ToolGroup`s through
`AgentToolRegistry` before startup. The Agent startup hook starts that complete
Tool Registry before spawning the runtime task.

At task startup, Agent reads its scoped `/resources/workflows.json` and loads
each definition transiently through the required `WorkflowService` capability,
after all Plugins have registered their Actions. Bundled definitions remain in
`/resources` and are never copied into the user-created
`/data/workflows.json` catalog. A persisted user definition with the same ID
takes precedence. A missing or invalid resource, or a rejected definition, is
logged and stops the Agent task before it begins driving sessions.

The shared `WebServer` capability continues to own `POST /api/model-api`.
The retained route registration is released automatically when the Agent Plugin
unloads.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` with ID `agent`, title
`模型配置`, module `entry.js`, and a `ResourceFiles` provider. Unload removes
the navigation entry and resource provider; no extra HTTP route is registered.

Cargo automatically runs the declared `build` task before compiling this Plugin.
To build only its resources, run `cargo plugin run build --plugin agent`
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
