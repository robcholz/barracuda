# File Plugin JSON RPC

Both methods are unary JSON RPCs with visibility `"*"`. Paths and file contents
are UTF-8 and resolve inside the File Plugin's private filesystem namespace.

## `file.read`

- Request schema: `schemas/rpc/read/request.json`
- Response schema: `schemas/rpc/read/response.json`
- Maximum encoded request: 512 bytes
- Maximum encoded response: 512 bytes

Request:

```json
{"path":"state/notes.txt"}
```

Success:

```json
{"content":"first line\nsecond line"}
```

## `file.write`

- Request schema: `schemas/rpc/write/request.json`
- Response schema: `schemas/rpc/write/response.json`
- Maximum encoded request: 512 bytes
- Maximum encoded response: 29 bytes

Request:

```json
{"path":"state/notes.txt","content":"first line\nsecond line"}
```

Success has no result payload:

```json
{}
```

Path and content have no independent byte quotas. For `file.write`, both share
one 512-byte request lane and one lane-sized decoding scratch buffer. JSON
escapes are decoded before writing, so newlines, quotes, backslashes, and
Unicode escapes behave as normal JSON strings.

## Business errors

Both methods may return:

```json
{"error":"invalid_request"}
{"error":"not_found"}
{"error":"permission_denied"}
{"error":"io"}
```

`file.read` may additionally return:

```json
{"error":"invalid_utf8"}
{"error":"too_large"}
```

`invalid_request` covers semantically invalid paths. A read can return up to 498
unescaped UTF-8 bytes, derived from the 512-byte response lane minus the
`{"content":""}` envelope. `too_large` covers larger stored files and responses
whose JSON-escaped form cannot fit the response lane. Malformed JSON, a request
exceeding the 512-byte transport bound, lane exhaustion, and other Event Router
framing failures are `RpcError` values rather than business documents.
