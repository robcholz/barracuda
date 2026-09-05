# Scheduler JSON RPC API

Both Scheduler operations are unary JSON RPCs with visibility `"*"`. Their
schemas are included directly from `plugins/scheduler/schemas/rpc`; there is no
native RPC endpoint, fixed-layout wire DTO, or schema-baking step.

The request is deserialized as a borrowed view of the RPC lane. Accepted IDs
are copied into fixed 16-byte storage, responses are written directly into the
lane, and each successful mutation atomically replaces or deletes only that
schedule's 32-byte KV value. The collection has no fixed schedule count.

Schedule IDs contain 1–16 ASCII letters, digits, `_`, `-`, or `.`. The ID is
also the Topic of each resulting `scheduler.triggered` Event.

## `scheduler.schedule`

- Visibility: `"*"`
- Maximum request: 512 bytes
- Maximum response: 32 bytes
- Request schema: `schemas/rpc/schedule/request.json`
- Response schema: `schemas/rpc/schedule/response.json`

The request contains a caller-selected `id` and a `trigger`. `at` uses the same
24-byte RFC3339 UTC representation returned by `time.now.utc`, including exactly
three fractional digits and the trailing `Z`. Scheduler evaluates deadlines at
whole-second precision; the fractional digits do not create subsecond timing:

```json
{
  "id": "meeting",
  "trigger": {
    "type": "once",
    "at": "2027-01-15T08:30:00.000Z"
  }
}
```

An interval trigger adds `every_seconds` and `count`. `count` includes the
first occurrence and both values must be non-zero.

```json
{
  "id": "drink-water",
  "trigger": {
    "type": "interval",
    "at": "2027-01-15T08:30:00.000Z",
    "every_seconds": 3600,
    "count": 3
  }
}
```

Success returns the accepted identity:

```json
{"id":"drink-water"}
```

Business failures are response documents:

| Error | Meaning |
| --- | --- |
| `invalid_schedule` | The ID, UTC timestamp, or trigger rule is invalid. |
| `duplicate_id` | A live schedule already owns the requested ID. |
| `time_unavailable` | The `UtcClock` capability is unsynchronized, stale, or out of range. |
| `trigger_in_past` | `at` is earlier than the current UTC clock value. |
| `storage_unavailable` | The affected schedule KV value could not be committed. |

For example: `{"error":"duplicate_id"}`.

## `scheduler.cancel`

- Visibility: `"*"`
- Maximum request: 512 bytes
- Maximum response: 64 bytes
- Request schema: `schemas/rpc/cancel/request.json`
- Response schema: `schemas/rpc/cancel/response.json`

Request:

```json
{"id":"drink-water"}
```

Success returns the cancelled identity and the number of Events accepted
before cancellation:

```json
{"id":"drink-water","completed_runs":1}
```

`{"error":"not_found"}` means no live schedule owns the ID.
`{"error":"invalid_schedule"}` means the supplied ID violates the Scheduler
ID rules. An Event already accepted by Event Router cannot be recalled;
cancellation prevents subsequent occurrences.
`{"error":"storage_unavailable"}` means cancellation was rolled back because
its schedule key could not be deleted.

Invalid JSON and documents with the wrong request shape are transport errors
reported as `RpcError::InvalidJson`, not business response documents.
