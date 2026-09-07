# Agent Workflow Actions

The Agent Plugin registers its session operations directly through
`WorkflowActionRegistry`. There is no RPC adapter, lane, frame size, or encoded
request/response byte limit on this path. Static request and response schemas
are compiled together with their validators, and the Action adapter uses owned
Serde request and response types.

Existing Workflow behavior is retained: stable business failures are successful
Action responses with `{"error":"code"}`. An operation that already parsed a
session may retain it as `{"session":"session-N","error":"code"}`. This lets
existing definitions branch on `$previous.output.error`. An
`WorkflowActionError` is reserved for Action infrastructure failures rather than
these established business outcomes.

| Address | Request | Success | Stable business errors |
| --- | --- | --- | --- |
| `session.new` | `{"persistence":"persistent" \| "ephemeral"}` | `{"session":"session-N"}` | `worker_stopped`, `persistence` |
| `session.list` | `{"offset"?:0,"limit"?:positive integer}` | `{"sessions":[...],"next_offset":number \| null}` | `invalid_request` |
| `session.open` | `{"session":"session-N"}` | `{"session":"session-N","run":"run-N"}` | `invalid_request`, `session_not_found`, `already_open`, `worker_stopped` |
| `session.delete` | `{"session":"session-N"}` | `{}` | `invalid_request`, `session_not_found`, `already_deleting`, `worker_stopped`, `storage` |
| `session.append` | `{"session":"session-N","text":"..."}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` |
| `session.respond` | `{"session":"session-N","request":"input-N","text":"..."}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `not_awaiting_input`, `input_request_mismatch`, `worker_stopped` |
| `session.set_reasoning_effort` | `{"session":"session-N","effort":"low" \| "medium" \| "high" \| "ultra"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` |
| `session.set_permission_level` | `{"session":"session-N","level":"deny" \| "ask" \| "allow_all"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` |
| `session.interrupt` | `{"session":"session-N"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` |
| `session.cancel` | `{"session":"session-N"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` |
| `session.close` | `{"session":"session-N"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` |

Omitting `session.list.limit` returns every remaining session. Supplying a limit
retains the existing pagination contract without the former 16-session cap.

`session.open` establishes the control lease and Event subscription used by the
other session Actions. `session.close` acknowledges the command; the matching
`closed` or `stream_error` Event is the authoritative end of that lease.

Schemas live at
`schemas/action/<address>/{request,response}.json`.
