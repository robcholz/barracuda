# Agent Component

The Agent Event Router Component owns the `RuntimeService` future and exposes
the existing `AgentRuntime` and `SessionControl` APIs as typed RPCs. It does not
know about Gateway events, IM routes, or any Gateway–Agent adapter.

The Rust module layout follows API ownership:

- root RPC modules correspond to methods on `AgentRuntime`;
- `session/` contains only methods on `SessionControl`;
- `open_session.rs` is therefore parallel to `session/`, not inside it;
- `lib.rs` only declares public RPC modules; it does not flatten them with
  re-exports.

Every RPC file also exports its reusable `*_handler` constructor. The
Component's `component.rs` contains only shared ownership, registration, and
lifecycle code; it does not contain RPC business handlers.

## AgentRuntime RPCs

| Address | Input | Output | Method error |
| --- | --- | --- | --- |
| `agent.link_api` | streaming `LinkApiRequestFrame` | unary `()` | `LinkApiError` |
| `agent.new_session` | unary `NewSessionRequest` | unary `NewSessionResponse` | `NewSessionError` |
| `agent.list_sessions` | unary `()` | streaming `ListSessionsResponse` | `()` |
| `agent.open_session` | unary `OpenSessionRequest` | streaming `OpenSessionResponseFrame` | `OpenSessionError` |
| `agent.delete_session` | unary `DeleteSessionRequest` | unary `()` | `DeleteSessionError` |

`new_session`, `list_sessions`, `open_session`, and `delete_session` use normal
fixed-layout Rust structs. Their public constructors and accessors use the
Agent domain types such as `SessionId` and `SessionPersistence`; local DTOs
exist only where an upstream type does not implement Event Router's fixed-wire
derives.

`link_api` uses frames because `ModelApiConfig` contains caller-sized strings.
`open_session` uses response frames because Agent events can contain
unpredictably sized text, tool arguments, tool output, and error messages. The
first logical response is `OpenSessionResponse::Opened`; subsequent responses
preserve the complete session-event stream as `SessionEventDto` values.

## SessionControl RPCs

Each contract remains in its own module, for example
`barracuda_agent_component::session::respond::Respond`.

| Address | Input | Output | Method error |
| --- | --- | --- | --- |
| `session.append` | streaming `AppendRequestFrame` | unary `()` | `SessionRpcError` |
| `session.respond` | streaming `RespondRequestFrame` | unary `()` | `SessionRpcError` |
| `session.set_reasoning_effort` | unary `SetReasoningEffortRequest` | unary `()` | `SessionRpcError` |
| `session.set_permission_level` | unary `SetPermissionLevelRequest` | unary `()` | `SessionRpcError` |
| `session.interrupt` | unary `InterruptRequest` | unary `()` | `SessionRpcError` |
| `session.cancel` | unary `CancelRequest` | unary `()` | `SessionRpcError` |
| `session.close` | unary `CloseRequest` | unary `()` | `SessionRpcError` |

Only `append` and `respond` use request frames because `Message` contains
unpredictably sized text. The other five control requests are ordinary typed
structs. A successful `close` removes the Component's cached control handle;
closed Agent event streams remove it as well.

## Emits

This Component emits no Events. Agent output belongs to the streaming
`agent.open_session` RPC contract.

## Lifecycle

`AgentComponent::new(runtime, service)` accepts the two values returned by the
Agent runtime constructor. `run` cooperatively drives `RuntimeService`.
Unloading clears cached open-session controls, while Event Router owns RPC
unregistration.
