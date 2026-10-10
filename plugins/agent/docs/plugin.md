# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: `webserver`, `workflow`, `captive-portal`
- Provided typed capabilities: `AgentRuntime`, `AgentToolRegistry`
- Required typed capabilities: `WebServer` from `webserver`,
  `WorkflowActionRegistry` and `WorkflowService` from `workflow`, and
  `CaptivePortal` from `captive-portal`
- Agent Tools: `skill_list`, `skill_read`, `skill_resource_read`, `skill_reload`,
  plus the runtime's mode, memory, plan, profile, conversation, and tool-loading Tools
- Filesystem: private; reads bundled `/resources/workflows.json`, user-installed
  skills from `/workspace/media/skills`, and shared bundled skills from
  `/workspace/resources/skills`
- Storage: model API records under the Plugin-scoped KV keys `default` and
  `purpose.{root_agent,sub_agent,memory,compaction}`
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

The runtime scans `/workspace/media/skills` and `/workspace/resources/skills`
into one catalog. Skills belong to the Agent application rather than the
framework, so this is an Agent convention over existing Workspace mounts: any
Plugin that writes files, such as `agent-file`, installs a skill by writing its
package under `/workspace/media/skills`. Skill names are globally unique
across both roots; their filesystem location has no selection priority. A
malformed package, or every copy of a duplicated name, is left out with a
warning and reported by `skill_reload`, so one bad package in the shared
directory never stops the Agent. `skill_read` loads only
the selected `SKILL.md` instructions. `skill_resource_read` resolves a bounded
UTF-8 file path relative to that unique skill directory without exposing the
backing path to the model.

At task startup, Agent reads its scoped `/resources/workflows.json` and loads
each definition transiently through the required `WorkflowService` capability,
after all Plugins have registered their Actions. Bundled definitions remain in
`/resources` and are never copied into the user-created
`/data/workflows.json` catalog. A persisted user definition with the same ID
takes precedence. A missing or invalid resource, or a rejected definition, is
logged and stops the Agent task before it begins driving sessions.

The shared `WebServer` capability continues to own `POST /api/model-api` and
`GET /api/model-api/status` (see `http.md`); both read the same in-memory
configuration. The retained route registrations are released automatically
when the Agent Plugin unloads. Accepted model configurations are atomically persisted before they
become active. Registration restores the complete model, purpose-binding, and
default-model snapshot before Agent work starts. Missing storage yields an empty
configuration; malformed stored data fails Plugin registration.

The portal entry is registered with `CaptivePortal::register_with_status`; its
`GET /portal/status` record is `EntryStatus::configured`: `ready`
已配置/Configured while the active configuration resolves a model for at least
one purpose (explicitly or through the default), otherwise `off` 未配置/Not set
up. It reads a flag the endpoint updates after restore and each accepted
`POST`.

## Portal page

Requires `CaptivePortal` from `captive-portal` and a private Plugin filesystem
scope. Registration retains a `WebEntryRegistration` for a `WebEntry` with ID
`agent`, group `WebGroup::Agent`, order 10, title `模型配置` / `Models`, a bilingual
summary, icon `icon.svg`, figure `figure.js`, module `entry.js`, and a
`ResourceFiles` provider. Unload removes the navigation entry and resource
provider; no extra HTTP route is registered.

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

The page is built on the portal UI kit: a header with the live `socket` figure
(`resources/web/figure.js`, using the shell's global `HL`), radio cards for the
API format (`backend`) and purpose, the 设为默认模型 switch (`default`),
connection fields (`base_url`, `model`, `api_key`) and an open 高级 fold with
`timeout_ms`, `max_tokens` and `image_max_bytes`. It sends every field the
endpoint requires, as a one-element batch, and renders zh or en from the
portal language. `resources/web/icon.svg` is the Lucide `cpu` mark.

The page submits the existing POST configuration contract and, once the device
accepts, calls `context.refreshStatus()` so the portal's 「开始使用」 step and
navigation follow. Its header shows the 状态 badge and, for each purpose, the
model it uses (its own, or the default), read from `GET /api/model-api/status`
on mount and after each accepted save. It does not claim that acceptance
verifies the upstream service. Secrets
are password inputs, never persisted in browser storage, and cleared on success
or unmount. Requests are cancelled on unmount and are never retried automatically.
The existing HTTP API has no authentication or transport encryption added here;
use only within a trusted provisioning network.
