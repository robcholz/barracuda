# Agent JSON Events

## `session.event`

`session.open` establishes one event subscription. Every emitted record has one
small, stable envelope:

```json
{
  "session": "session-1",
  "sequence": 3,
  "type": "output_delta",
  "payload": { "text": "hello" }
}
```

- `session` identifies the source session.
- `sequence` is the Agent Workflow adapter's monotonically increasing output order.
  Consumers use it to restore order when separate Workflow executions overlap.
- `type` identifies the semantic event.
- `payload` is an event-specific object. Marker events use `{}`.

There is no separate run, field, chunk-index, completion, or terminal metadata.
Explicit semantic events carry those boundaries: `reasoning_ended`,
`output_ended`, `tool_result_ended`, `turn_ended`, `closed`, and
`stream_error`.

### Event types

Turn and iteration lifecycle records are:

- `turn_started`: `{ "turn", "origin" }`, where origin is `user` or
  `tool_call`.
- `turn_origin_tool_call_id_delta`, `turn_origin_tool_name_delta`, and
  `turn_origin_arguments_delta`: `{ "text" }` fragments for a tool-created
  turn, followed by `turn_origin_ended` with `{}`.
- `iteration_started`: `{ "iteration" }`.
- `iteration_ended`: `{}`.
- `turn_ended`: `{ "turn" }`.

Model and effect output records are:

- `reasoning_delta`, `output_delta`, and `effect_output_delta`:
  `{ "text" }`.
- `reasoning_ended`, `output_ended`, and `effect_output_ended`: `{}`.
- `usage`: any counters reported by the provider as numeric
  `input_tokens`, `output_tokens`, `cache_read_tokens`, and
  `cache_write_tokens` properties.

One tool result is represented by `tool_result_started`, zero or more
`tool_call_id_delta`, `tool_name_delta`, `tool_arguments_delta`, and
`tool_output_delta` records, then `tool_result_ended` with `{ "ok": boolean }`.
`tool_results_ended` marks the end of all tool results for the iteration.

One permission request is represented by `input_request_started` with
`{ "request", "kind": "permission_approval" }`, its
`input_request_tool_call_id_delta`, `input_request_tool_name_delta`,
`input_request_arguments_delta`, and `input_request_reason_delta` text records,
then `input_requested` with `{ "request" }` when it is ready for a response.

Errors and closure records are:

- `turn_error` and `session_error`: `{ "message", "message_truncated" }`.
- `closed`: `{ "reason" }`.
- `stream_error`: `{ "error" }`.

### Direct deltas

The adapter forwards every runtime delta directly as one Workflow Event. It
does not split text to fit a transport frame and does not truncate error text.
Consumers still concatenate consecutive semantic delta records because the
model runtime itself may produce output incrementally.

The authoritative schema is `schemas/event/session_event.json`. It fixes the
four envelope properties while intentionally leaving each payload's contents
to its event type, so adding a new semantic event does not require changing
Workflow wiring.
