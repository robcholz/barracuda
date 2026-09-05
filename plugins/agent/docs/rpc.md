# Agent JSON RPCs

Every Agent RPC is unary JSON, registered with visibility `"system"`. A business
failure is a successful transport response with `{"error":"code"}`. `RpcError`
is reserved for invalid JSON/framing, lane capacity, or Event Router runtime
failure. Successful commands without response data return `{}`.

| Address | Request | Success | Stable business errors | Request/response bytes |
| --- | --- | --- | --- | --- |
| `session.new` | `{"persistence":"persistent" \| "ephemeral"}` | `{"session":"session-N"}` | `worker_stopped`, `persistence` | 64 / 32 |
| `session.list` | `{"offset"?:0,"limit"?:1..16}` | `{"sessions":[...],"next_offset":number \| null}` | `invalid_request` | 64 / 512 |
| `session.open` | `{"session":"session-N"}` | `{"session":"session-N","run":"run-N"}` | `invalid_request`, `session_not_found`, `already_open`, `worker_stopped` | 48 / 64 |
| `session.delete` | `{"session":"session-N"}` | `{}` | `invalid_request`, `session_not_found`, `already_deleting`, `worker_stopped`, `storage` | 48 / 31 |
| `session.append` | `{"session":"session-N","text":"..."}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` | 512 / 34 |
| `session.respond` | `{"session":"session-N","request":"input-N","text":"..."}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `not_awaiting_input`, `input_request_mismatch`, `worker_stopped` | 512 / 34 |
| `session.set_reasoning_effort` | `{"session":"session-N","effort":"low" \| "medium" \| "high" \| "ultra"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` | 80 / 34 |
| `session.set_permission_level` | `{"session":"session-N","level":"deny" \| "ask" \| "allow_all"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` | 80 / 34 |
| `session.interrupt` | `{"session":"session-N"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` | 48 / 34 |
| `session.cancel` | `{"session":"session-N"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` | 48 / 34 |
| `session.close` | `{"session":"session-N"}` | `{}` | `invalid_request`, `session_not_open`, `session_closed`, `worker_stopped` | 48 / 34 |

`session.list` is a bounded snapshot page. Omitted `offset` and `limit` default
to `0` and `16`; pass `next_offset` into the following call until it is `null`.

`session.open` is a command, not a transport stream. Its `run` identifies that
particular open lease. Runtime output is delivered separately through the
bounded `session.event` contract. `session.close` acknowledges the command;
the matching terminal Event is the authoritative end of that run.

`text` is ordinary JSON UTF-8 text with no independent field limit. The complete
encoded request, including escaping and the other fields, must fit the RPC's
512-byte lane. The handler borrows request fields from the lane. It allocates
only the `Message` data that `SessionControl` must own after the RPC lane is
released.

Schemas live at `schemas/rpc/<short-name>/{request,response}.json` and are
included by each method's `JsonRpcSchema` implementation.
