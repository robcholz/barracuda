# Agent JSON Events

## `session.event`

`session.open` establishes one event subscription and returns its `session`
and `run`. The Agent Component then emits every logical runtime event as one or
more bounded field chunks:

```json
{
  "session": "session-1",
  "run": "run-1",
  "sequence": 3,
  "chunk_index": 1,
  "field": "text",
  "chunk": "bounded UTF-8",
  "field_complete": true,
  "event_complete": true,
  "terminal": null
}
```

- `sequence` identifies the logical runtime event within the open run.
- `chunk_index` orders every field chunk within that logical event.
- `field` names the reconstructed field; concatenate its chunks in
  `chunk_index` order.
- `chunk` uses the remaining capacity of the complete Event Router lane after
  its envelope and metadata. It never splits a UTF-8 code point or JSON escape.
- `field_complete` marks the final chunk for that field.
- `event_complete` marks the final field chunk for the logical event.
- `terminal` is non-null only on the final chunk of the run. It is exactly one
  of `closed` or `worker_stopped`.

The first reconstructed field is `type`. Values are `turn_started`,
`input_requested`, `iteration_started`, `reasoning_delta`, `reasoning_ended`,
`output_delta`, `output_ended`, `tool_result`, `tool_results_ended`,
`iteration_ended`, `usage`, `effect_output_delta`, `effect_output_ended`,
`turn_error`, `turn_ended`, `session_error`, `closed`, and `stream_error`.
Subsequent fields depend on the type and include IDs, `origin`, `kind`, `text`,
`output`, `ok`, `reason`, and `message`. A `usage` event preserves every
provider counter that is present as `input_tokens`, `output_tokens`,
`cache_read_tokens`, or `cache_write_tokens`.

Tool calls use `tool_call_id`, `tool_name`, and `tool_arguments_json`.
`tool_arguments_json` is the Agent's original raw JSON string; consumers join
its chunks before interpreting it, so the Event layer neither creates a
`serde_json::Value` nor changes its JSON semantics.

The provider does not serialize a whole logical event into a temporary owned
JSON buffer. It moves runtime-owned strings into a short-lived event document,
borrows one field at a time, and writes each envelope directly into an Event
Router lane. Error display text is copied into one fixed 192-byte buffer and
reports `message_truncated` when necessary. This keeps per-emission storage
bounded while allowing arbitrarily long runtime fields to flow through a
sequence of fixed-size chunks.

The authoritative schema is `schemas/event/session_event.json`.
