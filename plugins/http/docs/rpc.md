# HTTP RPC API

## `http.request`

- Address: `http.request`
- Visibility: `*`
- Transport: unary JSON request to unary JSON response
- Request limit: 512 encoded bytes
- Response limit: 512 encoded bytes
- Request schema: `schemas/rpc/request/request.json`
- Response schema: `schemas/rpc/request/response.json`

Request:

```json
{
  "method": "POST",
  "url": "https://example.com/items",
  "headers": [
    { "name": "Content-Type", "value": "application/json" }
  ],
  "body": "{\"name\":\"example\"}"
}
```

`method` and `url` are required. `headers` defaults to `[]`, and `body`
defaults to `""`. Supported methods are `GET`, `POST`, `PUT`, `PATCH`,
`DELETE`, and `HEAD`. URL, ordered headers, and body do not have independent
byte quotas; the complete encoded request shares the 512-byte RPC lane.

Success response:

```json
{"status":201,"body":"created"}
```

The numeric `status` preserves the upstream HTTP response status. The Plugin
collects at most 488 response-body bytes and requires UTF-8. JSON escaping may
expand those bytes; if the complete success document would exceed the 512-byte
response lane, the Plugin returns `response_too_large` instead.

Stable operation failures are response documents:

```json
{"error":"invalid_url"}
```

| Error | Meaning |
| --- | --- |
| `invalid_url` | The URL is not absolute HTTP/HTTPS. |
| `tls_not_configured` | HTTPS was requested without Platform TLS. |
| `invalid_header` | A header violates its name or value rules. |
| `transport` | DNS, TCP, TLS, HTTP, or upstream body reading failed. |
| `invalid_response_text` | The upstream response body is not UTF-8. |
| `response_too_large` | The body or encoded success document exceeds its bound. |

Malformed JSON, a document that does not match the request shape, and an encoded
document larger than 512 bytes fail the RPC with `RpcError`; they are not HTTP
operation error documents. JSON escapes are decoded into one shared 512-byte
scratch buffer, and fields retain only ranges into it. Nested JSON text, Unicode
escapes, and control escapes therefore do not allocate heap strings.
