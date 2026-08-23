# Agent Component RPCs

The Agent Component exposes the existing Agent runtime and session control as
Event Router RPCs. Fixed-layout request and response types live in the
`dto` module and are re-exported from the RPC modules.

RPCs marked **dynamic** carry `#[rpc_dynamic]`, so they are reachable through
`RpcClient::call_json` and expose per-field wire access for Workflow links.
Streaming RPCs carry unbounded text and are not dynamic.

| Address | Input | Output | Method error | Dynamic |
| --- | --- | --- | --- | --- |
| `agent.link_api` | unary `LinkApiRequest` | unary `()` | `LinkApiError` | yes |
| `agent.new_session` | unary `NewSessionRequest` | unary `NewSessionResponse` | `NewSessionError` | yes |
| `agent.delete_session` | unary `DeleteSessionRequest` | unary `()` | `DeleteSessionError` | yes |
| `session.set_reasoning_effort` | unary `SetReasoningEffortRequest` | unary `()` | `SessionRpcError` | yes |
| `session.set_permission_level` | unary `SetPermissionLevelRequest` | unary `()` | `SessionRpcError` | yes |
| `session.interrupt` | unary `InterruptRequest` | unary `()` | `SessionRpcError` | yes |
| `session.cancel` | unary `CancelRequest` | unary `()` | `SessionRpcError` | yes |
| `session.close` | unary `CloseRequest` | unary `()` | `SessionRpcError` | yes |
| `agent.list_sessions` | unary `()` | streaming `ListSessionsResponse` | `()` | yes |
| `agent.open_session` | unary `OpenSessionRequest` | streaming `OpenSessionResponseFrame` | `OpenSessionError` | yes |
| `session.append` | streaming `AppendRequestFrame` | unary `()` | `SessionRpcError` | yes |
| `session.respond` | streaming `RespondRequestFrame` | unary `()` | `SessionRpcError` | yes |

## Text representation

Text is always a fixed-capacity, NUL-terminated UTF-8 C-string (`FixedStr`)
that serializes as a JSON string — never as a byte array.

## Request and response shapes

### `agent.link_api`

`LinkApiRequest` registers one LLM API configuration:

- `backend`: `"openai_compatible"` or `"anthropic_compatible"`
- `api_key`: string, up to 127 bytes
- `model`: string, up to 127 bytes
- `base_url`: string, up to 128 bytes
- `timeout_ms`: `u32`
- `max_tokens`: `u32`
- `image_max_bytes`: `u32`
- `purpose`: `"root_agent"`, `"sub_agent"`, `"memory"`, or `"compaction"`
- `default`: `bool`

### `agent.new_session`

`NewSessionRequest` carries `persistence` (`"persistent"` or `"ephemeral"`).
`NewSessionResponse` returns the created `session` identifier (`"session-N"`).

### `agent.list_sessions`

The response streams `ListSessionsResponse` items, each carrying up to four
session identifiers in a `sessions` array plus a `count`. Unused slots
serialize as `"session-0"`; callers should read only the first `count` entries.

### `agent.open_session`

`OpenSessionRequest` carries the `session` identifier. The response is a
streaming sequence of `OpenSessionResponseFrame` values, one per event. Each
frame carries the typed `session` identifier and `json`: the complete logical
event (`OpenSessionResponse`) as a JSON document.

Events: `opened`, `turn_started`, `input_requested`, `iteration_started`,
`reasoning_delta`, `reasoning_ended`, `output_delta`, `output_ended`,
`tool_result`, `tool_results_ended`, `iteration_ended`, `usage`,
`effect_output_delta`, `effect_output_ended`, `turn_error`, `turn_ended`,
`session_error`, and `closed`.

### `session.append` and `session.respond`

Each streamed `AppendRequestFrame` carries `session` and a fixed-capacity
`text` C-string; `RespondRequestFrame` additionally carries the `request`
identifier being answered. Each frame is one complete message (bounded to 399
bytes of text); a call may stream several.

### Session control requests

`SetReasoningEffortRequest`, `SetPermissionLevelRequest`, `InterruptRequest`,
`CancelRequest`, and `CloseRequest` are fixed-layout unary requests that carry
the target `session` identifier and, where applicable, the new value.

## Error reference

- `LinkApiError::InvalidConfiguration` — the linked API config is invalid.
- `NewSessionError::{WorkerStopped, Persistence}`.
- `DeleteSessionError::{SessionNotFound, AlreadyDeleting, WorkerStopped, Storage}`.
- `OpenSessionError::{SessionNotFound, AlreadyOpen, WorkerStopped, InvalidEvent}`.
- `SessionRpcError::{SessionNotOpen, SessionClosed, NotAwaitingInput,
  InputRequestMismatch, WorkerStopped, InvalidRequest}`.
