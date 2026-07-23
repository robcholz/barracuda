# Runtime Migration Decisions vs `master`

This note records the intentional compatibility boundary for replacing the
`master` C agent path with the Rust workspace under `framework/`. It describes
the current app wiring, not a future compatibility wish list.

## Architecture

`master` does **not** expose its main agent as a capability. Its Event Router
submits `RUN_AGENT` directly to `claw_core`; `cap_agent_mgr` is a separate set
of root-only subagent-management tools.

The migrated app uses a different boundary:

- Rust `AgentSystem` is constructed, started, stopped, and destroyed through
  `claw-cabi`.
- The C capability registry contains a system-only `agent` entry point. It is
  deliberately not marked `CLAW_CAP_FLAG_CALLABLE_BY_LLM`, so the model cannot
  recursively invoke itself.
- Event Router `RUN_AGENT` calls `claw_cap_call("agent", ...)` and no longer
  knows about an agent implementation.
- IM adapters call the shared IM session layer before publishing ordinary text.
  That layer owns `channel + chat_id -> selected_session_id` and the pending
  request last delivered to the chat; it writes the resulting numeric IDs into
  the event. `/session` action parsing remains in `cap_agent`.
- Event Router is stateless with respect to sessions. A normal `RUN_AGENT`
  forwards the event's IDs as structured `session + input`; explicit system
  callers may still supply a structured numeric-session operation.
- `cap_agent` owns one event pump per AgentSystem stream that belongs to this
  integration. Model reasoning stays internal; completed output streams are
  delivered to the accepted turn's target channel.

The app registers and starts its C capability groups before constructing
`AgentSystem`. Construction snapshots the enabled, LLM-visible C capabilities
as Rust tools. The `agent` system entry point is excluded from that tool set by
its flags.

## Completed Migration Surface

### Runtime lifecycle and API configuration

The public C ABI distinguishes four operations:

- `claw_agent_init`: constructs one stopped `AgentSystem` and restores its
  runtime state. It may start without an LLM binding when all initial API fields
  are blank.
- `claw_agent_start`: activates the existing system's tools and enables session
  operations. It does not reconstruct the system.
- `claw_agent_stop`: deactivates tools and gates session operations while
  retaining the `AgentSystem`, bindings, sessions, and open streams.
- `claw_agent_deinit`: stops if necessary and destroys the retained system.

`claw_agent_link_api` links or replaces a model binding on the retained system.
App configuration hot updates use this API; no old `claw_core` configuration
pointer is retained.

### Sessions and events

The C ABI uses numeric `uint32_t` session IDs and exposes create, list, open,
submit, respond, interrupt, cancel, receive, close, and delete operations.
Session creation requires the caller to choose `PERSISTENT` or `EPHEMERAL`.

The system-only `agent` capability exposes lifecycle actions
`create/open/close/delete/list`. Normal input uses one shape: `session_id` plus
`input.text`; an optional positive `input.request_id` selects `respond`,
otherwise the same input selects `submit`. `interrupt` and `cancel` remain
explicit control operations.

For an inbound IM message, the shared IM session layer resolves
`channel + chat_id` before publishing the event. The first ordinary message
creates and opens a persistent session. The event carries numeric `session_id`
and, when the previous `INPUT_REQUESTED` prompt was delivered successfully,
`request_id`. Router's `RUN_AGENT` forwards those IDs as the structured
`session + input` call. `cap_agent` never derives a session from chat context:
without a session, ordinary `RUN_AGENT` input is rejected instead of silently
falling back to a raw message.

IM keeps the `/session` user entry point with numeric IDs:
`new`, `list`, `switch <id>`, and `delete <id>`. The same `agent` capability
parses the full raw command, performs those operations, and updates the calling
chat's selection. Router only matches, forwards, and sends the returned text;
there is no separate `session_command` capability. A `/session` command does
not run through the ordinary auto-create/submit path.

The event ABI preserves the runtime event vocabulary:

- turn start/end and origin;
- input requests;
- iteration start/end;
- reasoning and output delta/end pairs;
- complete tool calls and tool-call end;
- error and closed.

Each event owns only the strings selected by its tagged union member, released
with `claw_agent_event_free`.

The device CLI uses only CLI-created numeric sessions. Every CLI session is
ephemeral, including `ask`, `ask_once`, and `session new`; `session <id>` may
switch only to an ephemeral session created by that CLI instance.

### HTTP streaming

`EspIdfHttp` implements buffered and streaming HTTP over one persistent
`esp_http_client_handle_t`. A streaming body borrows `&mut EspIdfHttp` for the
stream lifetime, so Rust prevents a buffered or second streaming request from
coexisting on that transport. EOF, cancellation, or dropping the stream
releases that same transport for reuse; no per-stream `EspClient` is created.

## Intentional Compatibility Breaks

These `master` surfaces are not restored:

- `/session` aliases and the old C session manager. The command remains, but
  it operates directly on global numeric AgentSystem session IDs. The shared
  IM session layer owns each chat's selected-session cursor; Router only
  forwards its numeric IDs and `cap_agent` calls the C ABI. CLI sessions use
  numeric IDs.
- `/llm` command capability. Configuration writes flow through the app and
  `claw_agent_link_api`.
- Old `cap_agent_mgr` tool names and the C `claw_manager` implementation. The
  Rust runtime owns its native subagent graph and tools.
- The `claw_core` request/response queue, context-provider callbacks, stage
  event surface, and request-id cancellation API.
- Binary compatibility with the previous `claw_agent_event_t`. There are no
  external precompiled consumers, so the ABI was upgraded directly without
  version/size fields or a parallel v2 API.
- Old C memory, skill-activation, and persistence file formats. The runtime
  uses its checkpoint, transcript, memory, and skill registries.

## Explicitly Deferred

- `cap_llm_inspect` still depends on the old C core media API and is excluded
  from `edge_agent`. A runtime-native media capability can be designed
  separately.
- Cross-reboot restoration of the IM
  `channel + chat_id -> selected_session_id`
  cursor is part of a later persistence pass. This does not change the session
  persistence choice exposed by the ABI.
- Broad documentation for the removed C architecture is migrated separately;
  it is not a reason to keep dead runtime dependencies in the application.

## Verification Boundary

The migration is considered wired when all of the following hold:

1. Host tests for `claw-interface`, `claw-api`, `claw-agent`, and `claw-cabi`
   pass.
2. `edge_agent` builds for ESP32-S3 with the Rust static library linked.
3. Device CLI can create an ephemeral numeric session and consume the new event
   stream.
4. A real Event Router message reaches the `agent` capability and its completed
   output is delivered through the requested IM capability.
