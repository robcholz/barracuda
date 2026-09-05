# Time JSON RPC API

The Time Plugin exposes synchronized UTC to Agents and Workflows through one
unary JSON RPC. Plugin-to-Plugin callers should use the typed `UtcClock`
capability instead.

## `time.now`

- Address: `time.now`
- Visibility: `*`
- Request limit: 2 bytes
- Response limit: 34 bytes
- Request schema: `plugins/time/schemas/rpc/now/request.json`
- Response schema: `plugins/time/schemas/rpc/now/response.json`

The request is exactly the empty JSON object:

```json
{}
```

A successful response contains an RFC 3339 UTC timestamp with fixed millisecond
precision:

```json
{"utc":"2026-09-05T14:30:00.123Z"}
```

The trailing `Z` identifies UTC. The RPC never substitutes uptime, zero, or a
firmware build timestamp when synchronized UTC is unavailable.

Clock availability failures are successful JSON RPC responses with one stable
business error:

```json
{"error":"unsynchronized"}
```

| Error | Meaning |
| --- | --- |
| `unsynchronized` | No valid network time sample has completed since boot. |
| `stale` | The last accepted sample is older than the configured maximum holdover. |
| `out_of_range` | The UTC value cannot be represented in the response timestamp format. |

Requests within the 2-byte limit that are not the exact empty object fail with
the Event Router's `RpcError::InvalidJson` transport error. Larger requests fail
with `RpcError::FrameTooLarge` before the handler runs. Neither transport failure
produces a business-error document.

The request and response schemas are included directly at compile time. This
contract has no native wire DTO, schema-only wire crate, build script, or
transport streaming cardinality.
