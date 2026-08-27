# Tavily RPC

## `tavily.search`

Dynamic unary-to-streaming RPC used by Agents to search the public web.

### Request: `TavilySearchRequest`

| Field | Type | Meaning |
| --- | --- | --- |
| `query` | JSON string / bounded UTF-8 wire text | Search query (at most 255 UTF-8 bytes). |
| `max_results` | `u8` | Requested result count from 1 through 10. |

### Response stream: `TavilySearchResult`

Each frame contains one result. Tavily text is UTF-8-boundary truncated to the
fixed RPC capacities so every result remains suitable for the Event Router's
bounded lanes.

| Field | Type | Meaning |
| --- | --- | --- |
| `title` | string | Page title. |
| `url` | string | Source URL. |
| `content` | string | Search excerpt. |
| `score` | `f32` | Tavily relevance score. |

### Method errors

| Variant | Meaning |
| --- | --- |
| `NotConfigured` | No Tavily configuration has been posted. |
| `InvalidRequest` | Query encoding or `max_results` is invalid. |
| `Transport` | DNS, TCP, TLS, HTTP, or response-body reading failed. |
| `Service` | Tavily returned a non-success HTTP status. |
| `InvalidResponse` | The response was too large or malformed. |
