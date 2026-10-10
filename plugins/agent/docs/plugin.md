# Agent Plugin

- Plugin ID: `agent`
- Direct Plugin dependencies: `webserver`, `workflow`, `captive-portal`
- Provided typed capabilities: `AgentRuntime`, `AgentToolRegistry`
- Required typed capabilities: `WebServer` from `webserver`,
  `WorkflowActionRegistry` and `WorkflowService` from `workflow`, and
  `CaptivePortal` from `captive-portal`
- Agent Tools: `skill_list`, `skill_read`, `skill_resource_read`, `skill_reload`,
  `background_list`, `background_wait`, `background_input`, `background_cancel`,
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

## Tools and background Tools

A Tool is either an ordinary Tool (`Tool::new` with a `ToolHandler`), whose call
settles inside the model turn, or a background Tool (`Tool::background` with a
`BackgroundToolHandler`), whose call returns an accepted output and keeps
running. Being a background Tool is part of the Tool's definition, not a choice
made per call.

Every Agent owns one `BackgroundToolPool`. The pool is a lower layer than
Tools: it is the generic `barracuda_runtime_utils::background::BackgroundPool`,
which stores tasks under pool-assigned ids beside metadata it never inspects,
delivers their progress and completion, and lets holders wait for or remove a
task by id. The Tool layer instantiates it with `BackgroundToolCall` metadata
(the invocation, the Tool's control hooks, and the `toolcall` span) and owns
every Tool-level behavior.

The runner moves an accepted background call into the pool, and its accepted
output becomes `[background:accepted]` with the pool `id`. The Agent drives the
pool and delivers the call's progress and terminal result automatically as
`[background:progress]`, `[background:completed]`, or `[background:failed]`
updates; an idle Agent opens a new turn for them. The always-visible
`background` Tool group manipulates the pool by `id`:

- `background_list` snapshots every running call with its Tool and status;
- `background_wait` blocks until one call finishes and returns its result,
  which is then not delivered again, or times out and leaves it running;
- `background_input` sends input or end of input to a running call;
- `background_cancel` drops a running call; its result is never delivered.

A handler attaches `BackgroundToolControl` to supply the status, accept input,
and stop work its dropped completion future does not own. Dropping a call
always drops its completion future, so cancelling, cancelling the Agent, or
dropping the Agent stops every call whose work that future owns. The pool is
runtime-only state and does not survive an Agent restart.

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

The shared `WebServer` capability continues to own `POST /api/model-api`.
The retained route registration is released automatically when the Agent Plugin
unloads. Accepted model configurations are atomically persisted before they
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
navigation follow. It does not read current settings or claim that acceptance
verifies the upstream service. Secrets
are password inputs, never persisted in browser storage, and cleared on success
or unmount. Requests are cancelled on unmount and are never retried automatically.
The existing HTTP API has no authentication or transport encryption added here;
use only within a trusted provisioning network.
